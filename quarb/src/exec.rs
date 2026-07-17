//! Executor: evaluate a [`Query`] against an [`AstAdapter`].
//!
//! Navigation maps a node context step by step. Once a projection
//! runs, the context becomes a list of *capsae* — each a node with a
//! register (breadcrumbs) and a current topic — that the pipeline
//! stages transform, aggregate, push, and recall.

use crate::adapter::{AstAdapter, NodeId};
use crate::ast::{
    Arg, ArithOp, Axis, Branch, CmpOp, FnCall, Group, InterpSeg, Matcher, Operand, PathElem,
    PredExpr, Predicate, Projection, PushBody, Query, Reach, RegRef, Stage, Step,
};
use crate::stdlib;
use crate::value::Value;
use regex::Regex;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// One entry in a capsa's register.
#[derive(Clone)]
struct Reg {
    name: Option<String>,
    value: Value,
}

/// One entry in a thread's mark store: a node labeled during
/// navigation (`.name` in node context), anchored on later with
/// `(name)`. Marks live beside the register — the register is a
/// stack of scalars, marks a stack of node handles — and never
/// enter the value space.
#[derive(Clone, PartialEq)]
struct Mark {
    name: String,
    node: NodeId,
}

/// The execution unit: a node, its register (breadcrumbs), and the
/// current topic (the scalar being worked on, once a projection has
/// run).
#[derive(Clone)]
struct Capsa {
    node: NodeId,
    register: Vec<Reg>,
    topic: Option<Value>,
    /// A group's member capsae (empty everywhere else). Carried by
    /// `@| group` alongside the list topic so keyed aggregates on
    /// `|` can work per group and `@| ungroup` can flatten back.
    members: Vec<Capsa>,
    /// The groups of the last successful `=~` match in a filter
    /// stage (`$1` … `$9`); empty until one matches.
    captures: Vec<String>,
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
    source: NodeId,
    label: String,
    target: NodeId,
}

/// The engine-reserved structural edge labels.
const CHILD: &str = "[child]";
const PARENT: &str = "[parent]";
const NEXT: &str = "[next]";
const PREV: &str = "[prev]";

/// The capsa scope a value expression may reach: the register (for
/// `$.name` recalls) and the topic (`$_`). Empty where no capsa
/// exists yet (navigation predicates).
#[derive(Clone, Copy)]
struct Scope<'a> {
    register: &'a [Reg],
    topic: Option<&'a Value>,
    /// 1-based position in the current context, when known.
    ordinal: Option<usize>,
    /// Regex captures from the last matching filter (`$1` …).
    captures: &'a [String],
    /// The marks in scope, for `(name)`-anchored paths.
    marks: &'a [Mark],
    /// The correlation witness that admitted this capsa (one node
    /// per trace context; null for an unmatched outer context),
    /// read back by `$*k` in pipeline stages.
    bindings: &'a [Option<NodeId>],
    /// The arrived-by edge (`$-`), where one is defined.
    edge: Option<&'a EdgeCtx>,
    /// All final-hop crossings (`@-`), for pipeline scopes.
    arrived: &'a [EdgeCtx],
    /// The stage's input context (`@*`), where one exists — the
    /// snapshot rule: a stage is the transition, so "the context"
    /// during its evaluation is what it received.
    peers: Option<&'a [Capsa]>,
    /// The invoking capsa's scope, one subcontext out (`$$.name`,
    /// `$$_`, `$$ord`); `None` at the top level.
    outer: Option<&'a Scope<'a>>,
}

const NO_SCOPE: Scope<'static> = Scope {
    register: &[],
    topic: None,
    ordinal: None,
    captures: &[],
    marks: &[],
    bindings: &[],
    edge: None,
    arrived: &[],
    peers: None,
    outer: None,
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
struct Correlation {
    /// Per-context outer flags (`<=>?`): an outer context may bind
    /// null when no real tuple admits a row.
    outer: Vec<bool>,
    contexts: Vec<Vec<NodeId>>,
    witnesses: std::cell::RefCell<std::collections::HashMap<u64, Vec<Option<NodeId>>>>,
}

type Trace = Correlation;

/// Evaluate `query` from the root.
/// Refuse a query that uses the `sh(...)` stage unless the adapter
/// allows it (`qua --allow-shell`). Query text stays inert data by
/// default — a `.quarb` file, a defs file, or a macro can never run
/// a command without the explicit per-run opt-in.
pub(crate) fn gate_shell(query: &Query, adapter: &impl AstAdapter) -> crate::Result<()> {
    if uses_shell_query(query) && !adapter.allow_shell() {
        return Err(crate::QuarbError::Unsupported(
            "the sh(...) stage runs external commands; pass --allow-shell".into(),
        ));
    }
    Ok(())
}

fn uses_shell_query(q: &Query) -> bool {
    q.correlations.iter().any(uses_shell_query)
        || q.branches
            .iter()
            .any(|b| b.steps.iter().any(uses_shell_elem))
        || q.pipeline.iter().any(uses_shell_stage)
}

fn uses_shell_stage(stage: &Stage) -> bool {
    let args = |call: &FnCall| {
        call.args.iter().any(|a| match a {
            Arg::Expr(e) => uses_shell_operand(e),
            _ => false,
        })
    };
    match stage {
        Stage::Func(call) | Stage::Agg(call) => call.name == "sh" || args(call),
        Stage::Subcontext { body, .. } => uses_shell_query(body),
        Stage::Map(inner) => uses_shell_stage(inner),
        Stage::Filter(e) => uses_shell_pred(e),
        Stage::Select(p) => match p {
            Predicate::Expr(e) => uses_shell_pred(e),
            _ => false,
        },
        Stage::Expr(o) | Stage::ExprPush { expr: o, .. } => uses_shell_operand(o),
        _ => false,
    }
}

fn uses_shell_pred(e: &PredExpr) -> bool {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => uses_shell_pred(a) || uses_shell_pred(b),
        PredExpr::Not(a) => uses_shell_pred(a),
        PredExpr::Compare(l, _, r) => uses_shell_operand(l) || uses_shell_operand(r),
        PredExpr::Truthy(o) => uses_shell_operand(o),
    }
}

fn uses_shell_elem(e: &PathElem) -> bool {
    match e {
        PathElem::Mark(_) => false,
        PathElem::Step(st) => st.predicates.iter().any(|p| match p {
            Predicate::Expr(e) => uses_shell_pred(e),
            _ => false,
        }),
        PathElem::Group(g) => {
            g.alts.iter().flatten().any(uses_shell_elem)
                || g.predicates.iter().any(|p| match p {
                    Predicate::Expr(e) => uses_shell_pred(e),
                    _ => false,
                })
        }
        PathElem::Push { body, .. } => match body {
            PushBody::Query(q) => uses_shell_query(q),
            PushBody::Expr(e) => uses_shell_operand(e),
        },
    }
}

fn uses_shell_operand(o: &Operand) -> bool {
    match o {
        Operand::Piped { expr, stages } => {
            uses_shell_operand(expr) || stages.iter().any(uses_shell_stage)
        }
        Operand::Neg(i) | Operand::Outer(i) => uses_shell_operand(i),
        Operand::Group(e) => uses_shell_pred(e),
        Operand::Arith { left, right, .. } => uses_shell_operand(left) || uses_shell_operand(right),
        Operand::Cond { cond, then, other } => {
            uses_shell_pred(cond) || uses_shell_operand(then) || uses_shell_operand(other)
        }
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            uses_shell_operand(scrutinee)
                || arms
                    .iter()
                    .any(|(t, _, r)| uses_shell_operand(t) || uses_shell_operand(r))
                || uses_shell_operand(other)
        }
        Operand::Interp(segs) => segs.iter().any(|seg| match seg {
            InterpSeg::Expr(e) => uses_shell_operand(e),
            InterpSeg::Text(_) => false,
        }),
        Operand::Rel { steps, .. } | Operand::Ctx { steps, .. } => {
            steps.iter().any(uses_shell_elem)
        }
        _ => false,
    }
}

pub fn eval(query: &Query, adapter: &impl AstAdapter) -> QueryResult {
    eval_query(query, adapter, adapter.root(), &Correlation::default())
}

/// Evaluate `query` from the root, returning the final capsae as
/// `(node, topic)` pairs — the provenance-bearing form of [`eval`]:
/// each result value still knows which node produced it. A `None`
/// topic is a node result (no projection ran).
pub fn eval_traced(query: &Query, adapter: &impl AstAdapter) -> Vec<(NodeId, Option<Value>)> {
    let (caps, projected) =
        eval_query_caps(query, adapter, adapter.root(), &Correlation::default());
    caps.into_iter()
        .map(|c| {
            let topic = if projected {
                Some(c.topic.unwrap_or(Value::Null))
            } else {
                c.topic
            };
            (c.node, topic)
        })
        .collect()
}

/// Evaluate `query` starting navigation from `start`, with `base`
/// correlation contexts already in scope. Prior `<=>` expressions are
/// evaluated first and appended to the trace.
fn eval_query(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
) -> QueryResult {
    eval_query_outer(query, adapter, start, base, None)
}

/// [`eval_query`] with the invoking capsa's scope, when evaluating
/// a subcontext body — what `$$.name` / `$$_` / `$$ord` reach.
fn eval_query_outer(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
    outer: Option<&Scope<'_>>,
) -> QueryResult {
    let mut contexts = base.contexts.clone();
    let mut outers = base.outer.clone();
    for corr in &query.correlations {
        // The left of `<=>` must be a node context.
        let prior = Correlation {
            contexts: contexts.clone(),
            outer: outers.clone(),
            witnesses: Default::default(),
        };
        let ctx = match eval_query(corr, adapter, start, &prior) {
            QueryResult::Nodes(ns) => ns,
            QueryResult::Values(_) => Vec::new(),
        };
        contexts.push(ctx);
        outers.push(corr.outer);
    }
    let trace = Correlation {
        contexts,
        outer: outers,
        witnesses: Default::default(),
    };
    let mut caps = union_branches(&query.branches, adapter, start, &trace, outer);
    for stage in &query.pipeline {
        caps = apply_stage(stage, caps, adapter, &trace, outer);
    }
    // Whether the query is value-typed is a static property: it holds
    // if any branch projects (`::`) or any pipeline stage produces
    // values. This must not depend on how many nodes matched\,---\,
    // `//a::text` over a document with no `<a>` is still an (empty)
    // list of values, not a node set. Node-preserving stages
    // (positional selection, keyed aggregates) keep a node context a
    // node context.
    to_result(caps, pipeline_projected(query))
}

/// [`eval_query`]'s capsa-level form: the final capsae plus the
/// static value-vs-node typing, before reduction to a result.
fn eval_query_caps(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
) -> (Vec<Capsa>, bool) {
    eval_query_caps_outer(query, adapter, start, base, None)
}

fn eval_query_caps_outer(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
    outer: Option<&Scope<'_>>,
) -> (Vec<Capsa>, bool) {
    let mut contexts = base.contexts.clone();
    let mut outers = base.outer.clone();
    for corr in &query.correlations {
        let prior = Correlation {
            contexts: contexts.clone(),
            outer: outers.clone(),
            witnesses: Default::default(),
        };
        let ctx = match eval_query(corr, adapter, start, &prior) {
            QueryResult::Nodes(ns) => ns,
            QueryResult::Values(_) => Vec::new(),
        };
        contexts.push(ctx);
        outers.push(corr.outer);
    }
    let trace = Correlation {
        contexts,
        outer: outers,
        witnesses: Default::default(),
    };
    let mut caps = union_branches(&query.branches, adapter, start, &trace, outer);
    for stage in &query.pipeline {
        caps = apply_stage(stage, caps, adapter, &trace, outer);
    }
    (caps, pipeline_projected(query))
}

/// Whether a pipeline stage turns the context value-typed. Positional
/// selection, per-capsa filters, and keyed aggregates reorder or
/// filter capsae without producing values; every other stage does.
fn stage_projects(stage: &Stage) -> bool {
    match stage {
        Stage::Select(_) | Stage::Filter(_) => false,
        // `group` produces list topics (value-typed); the other keyed
        // aggregates only reorder or filter. (`ungroup` is handled by
        // the fold in [`pipeline_projected`].)
        Stage::Agg(call) => !stdlib::known_keyed(&call.name) || call.name == "group",
        _ => true,
    }
}

/// The query's static value-vs-node typing, folded over the pipeline.
/// `@| group` and `@| window` set it (list topics); `@| ungroup`
/// restores whatever held before the matching member-maker — its
/// members carry the context as it was then. Other stages set it per
/// [`stage_projects`] and never clear it.
fn pipeline_projected(query: &Query) -> bool {
    let mut projected = query.branches.iter().any(|b| b.projection.is_some());
    let mut before_group: Vec<bool> = Vec::new();
    for stage in &query.pipeline {
        match stage {
            Stage::Agg(call) if matches!(call.name.as_str(), "group" | "window") => {
                before_group.push(projected);
                projected = true;
            }
            Stage::Agg(call) if call.name == "ungroup" => {
                projected = before_group.pop().unwrap_or(false);
            }
            s => projected = projected || stage_projects(s),
        }
    }
    projected
}

/// Navigate every branch from `start`, projecting where asked, and
/// union the resulting capsae (deduped by node).
fn union_branches(
    branches: &[Branch],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    let mut caps = Vec::new();
    for branch in branches {
        // An anchored branch (`^`) navigates from the root — the
        // same node at the top level, the reach-back from inside a
        // subcontext body. A `(name)`-anchored branch navigates
        // from the invoking thread's marked node; the invoker's
        // marks also seed the navigation, so nested predicates and
        // deeper anchors keep seeing them.
        let seed: &[Mark] = outer.map(|s| s.marks).unwrap_or(&[]);
        let from = if let Some(m) = &branch.mark {
            match seed.iter().rev().find(|k| &k.name == m) {
                Some(k) => k.node,
                None => continue,
            }
        } else if branch.anchored {
            adapter.root()
        } else {
            start
        };
        for (node, register, marks, arrived) in
            navigate_paths(&branch.steps, adapter, from, trace, outer, seed)
        {
            caps.push(Capsa {
                node,
                arrived,
                marks,
                // A pattern that pushed hands its breadcrumbs to the
                // result capsa; plain navigation hands an empty
                // register, exactly as before.
                register,
                topic: branch
                    .projection
                    .as_ref()
                    .map(|p| project(adapter, node, p)),
                members: Vec::new(),
                captures: Vec::new(),
                bindings: trace
                    .witnesses
                    .borrow()
                    .get(&node.0)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
    }
    // Dedup by (node, register): per-path results — the same node
    // reached along walks with different breadcrumbs — stay
    // distinct; without pushes every key is (node, empty) and the
    // old node dedup is preserved.
    let mut seen = HashSet::new();
    caps.retain(|c| seen.insert((c.node, reg_key(&c.register))));
    caps
}

/// Whether a stage's expressions read `@*` (directly or through a
/// pipe tail) — the trigger for materializing the input snapshot.
/// Subcontext bodies are excluded: their stages get contexts of
/// their own.
fn stage_reads_context(stage: &Stage) -> bool {
    fn op(o: &Operand) -> bool {
        match o {
            Operand::Capsae { .. } => true,
            Operand::Piped { expr, stages } => op(expr) || stages.iter().any(stage_reads_context),
            Operand::Arith { left, right, .. } => op(left) || op(right),
            Operand::Neg(inner) | Operand::Outer(inner) => op(inner),
            Operand::Group(e) => pred(e),
            Operand::Interp(segs) => segs.iter().any(|s| match s {
                InterpSeg::Expr(e) => op(e),
                InterpSeg::Text(_) => false,
            }),
            Operand::Rel { steps, .. } | Operand::Ctx { steps, .. } => steps.iter().any(elem),
            _ => false,
        }
    }
    fn pred(e: &PredExpr) -> bool {
        match e {
            PredExpr::Or(a, b) | PredExpr::And(a, b) => pred(a) || pred(b),
            PredExpr::Not(a) => pred(a),
            PredExpr::Compare(l, _, r) => op(l) || op(r),
            PredExpr::Truthy(o) => op(o),
        }
    }
    fn elem(e: &PathElem) -> bool {
        match e {
            PathElem::Mark(_) => false,
            PathElem::Step(st) => st.predicates.iter().any(|p| match p {
                Predicate::Expr(e) => pred(e),
                _ => false,
            }),
            PathElem::Group(g) => {
                g.alts.iter().flatten().any(elem)
                    || g.predicates.iter().any(|p| match p {
                        Predicate::Expr(e) => pred(e),
                        _ => false,
                    })
            }
            PathElem::Push { body, .. } => match body {
                PushBody::Expr(e) => op(e),
                PushBody::Query(_) => false,
            },
        }
    }
    match stage {
        Stage::Map(inner) => stage_reads_context(inner),
        Stage::Expr(e) | Stage::ExprPush { expr: e, .. } => op(e),
        Stage::Filter(e) => pred(e),
        Stage::Select(Predicate::Expr(e)) => pred(e),
        Stage::Func(call) | Stage::Agg(call) => call.args.iter().any(|a| match a {
            Arg::Expr(e) => op(e),
            _ => false,
        }),
        _ => false,
    }
}

/// Turn the final capsa context into a query result. `projected` is
/// the query's static value-vs-node typing (see [`eval_query`]); it
/// decides the empty case, where no capsa is left to inspect.
fn to_result(caps: Vec<Capsa>, projected: bool) -> QueryResult {
    if projected || caps.iter().any(|c| c.topic.is_some()) {
        QueryResult::Values(
            caps.into_iter()
                .map(|c| c.topic.unwrap_or(Value::Null))
                .collect(),
        )
    } else {
        QueryResult::Nodes(caps.into_iter().map(|c| c.node).collect())
    }
}

/// Apply one pipeline stage to the capsa context.
fn apply_stage(
    stage: &Stage,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    // The `@*` snapshot: materialized only when the stage actually
    // reads the context (the scan is cheap; the clone is not).
    let snapshot = stage_reads_context(stage).then(|| caps.clone());
    let peers = snapshot.as_deref();
    match stage {
        // `sh('cmd')` — the shell stage (gated: [`gate_shell`]
        // refused it long before we got here unless the adapter
        // allows it). The command — one argument, evaluated per
        // capsa, so an interpolated string parameterizes it — runs
        // under `sh -c`; the topic (as text) is its stdin, its
        // stdout (one trailing newline trimmed) the new topic; a
        // failing command yields null, propagating. stderr passes
        // through.
        // `| json` in a NODE context (no topic yet) serializes the
        // current node's subtree back to JSON — the encode half of
        // the node-mode json pair (decode(json) is the parse half).
        // With a topic present it stays the value serializer.
        Stage::Func(call) if call.name == "json" => caps
            .into_iter()
            .map(|mut c| {
                let v = match &c.topic {
                    Some(t) => Value::Str(t.to_json()),
                    None => Value::Str(node_to_json(adapter, c.node).to_json()),
                };
                c.topic = Some(v);
                c
            })
            .collect(),
        // `| xml` mirrors `| json`: a topic value serializes to
        // XML (record fields → elements, `@name` fields
        // attributes, `#text` the text; a list repeats an <item>);
        // a node context serializes the subtree.
        Stage::Func(call) if call.name == "xml" => caps
            .into_iter()
            .map(|mut c| {
                let xml = match &c.topic {
                    Some(t) => value_to_xml(t, "item"),
                    None => node_to_xml(adapter, c.node),
                };
                c.topic = Some(Value::Str(xml));
                c
            })
            .collect(),
        Stage::Func(call) if call.name == "sh" => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = Scope {
                    register: &c.register,
                    topic: c.topic.as_ref(),
                    ordinal: Some(i + 1),
                    captures: &c.captures,
                    marks: &c.marks,
                    bindings: &c.bindings,
                    edge: None,
                    arrived: &c.arrived,
                    peers,
                    outer,
                };
                let cmd = call.args.first().map(|a| match a {
                    Arg::Lit(v) => v.to_string(),
                    Arg::Expr(e) => operand_scalar(adapter, c.node, e, trace, scope).to_string(),
                    Arg::Range(..) => String::new(),
                });
                c.topic = Some(match cmd {
                    Some(cmd) if !cmd.is_empty() => {
                        run_shell(&cmd, c.topic.as_ref()).unwrap_or(Value::Null)
                    }
                    _ => Value::Null,
                });
                c
            })
            .collect(),
        // `record(...)` (alias `rec`) builds a record per capsa: expression fields
        // evaluate against the capsa's node, auto-named by their
        // projection or named by a preceding literal.
        Stage::Func(call) if matches!(call.name.as_str(), "record" | "rec") => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = Scope {
                    register: &c.register,
                    topic: c.topic.as_ref(),
                    ordinal: Some(i + 1),
                    captures: &c.captures,
                    marks: &c.marks,
                    bindings: &c.bindings,
                    edge: None,
                    arrived: &c.arrived,
                    peers,
                    outer,
                };
                let value = build_record(call, adapter, c.node, trace, scope);
                c.topic = Some(value);
                c
            })
            .collect(),
        // A keyed aggregate on the plain pipe works per capsa on the
        // capsa's *members* (a group's rows): reorder or filter them,
        // rebuild the list topic from the survivors. A memberless
        // capsa passes through unchanged.
        Stage::Func(call) if stdlib::known_keyed(&call.name) => caps
            .into_iter()
            .map(|mut c| {
                if c.members.is_empty() {
                    return c;
                }
                let members =
                    keyed_agg(call, std::mem::take(&mut c.members), adapter, trace, outer);
                c.topic = Some(Value::List(
                    members
                        .iter()
                        .map(|m| {
                            m.topic
                                .clone()
                                .unwrap_or_else(|| node_scalar(adapter, m.node))
                        })
                        .collect(),
                ));
                c.members = members;
                c
            })
            .collect(),
        // A reducing aggregate on the plain pipe reduces each capsa's
        // *list* topic (a group's members) — per-capsa work, so it
        // rides `|`. A non-list topic reduces as a singleton. The
        // reduction consumes the members: they no longer correspond
        // to the topic.
        Stage::Func(call) if stdlib::known_agg(&call.name) => caps
            .into_iter()
            .map(|mut c| {
                let items = match c.topic.take() {
                    Some(Value::List(items)) => items,
                    Some(other) => vec![other],
                    None => vec![node_scalar(adapter, c.node)],
                };
                let mut out = stdlib::apply(call, items);
                c.topic = Some(match out.len() {
                    1 => out.pop().expect("len checked"),
                    _ => Value::List(out),
                });
                c.members = Vec::new();
                c
            })
            .collect(),
        // Per-capsa scalar transform; a function may expand one topic
        // into several (e.g. `lines`), forking the capsa.
        Stage::Func(call) => caps
            .into_iter()
            .flat_map(|c| {
                let topic = c.topic.clone().unwrap_or(Value::Null);
                stdlib::apply_scalar(call, topic, &|e| adapter.unit_scale(e))
                    .into_iter()
                    .map(move |v| Capsa {
                        node: c.node,
                        register: c.register.clone(),
                        topic: Some(v),
                        members: Vec::new(),
                        captures: c.captures.clone(),
                        marks: c.marks.clone(),
                        bindings: c.bindings.clone(),
                        arrived: c.arrived.clone(),
                    })
            })
            .collect(),
        // The map pipe: the inner stage runs once per element of
        // the topic, per capsa — capsae unchanged, the list
        // reassembled (an expanding inner stage flattens). Inside,
        // `$_` is the element and `@*` reads the element-context.
        Stage::Map(inner) => caps
            .into_iter()
            .map(|mut c| {
                let (items, was_list) = match c.topic.take() {
                    Some(Value::List(items)) => (items, true),
                    Some(Value::Null) | None => {
                        // Null maps to null: nothing to map over.
                        c.topic = Some(Value::Null);
                        return c;
                    }
                    Some(other) => (vec![other], false),
                };
                let pseudo: Vec<Capsa> = items
                    .into_iter()
                    .map(|v| Capsa {
                        node: c.node,
                        register: c.register.clone(),
                        topic: Some(v),
                        members: Vec::new(),
                        captures: c.captures.clone(),
                        marks: c.marks.clone(),
                        bindings: c.bindings.clone(),
                        arrived: c.arrived.clone(),
                    })
                    .collect();
                let mut out: Vec<Value> = apply_stage(inner, pseudo, adapter, trace, outer)
                    .into_iter()
                    .map(|p| p.topic.unwrap_or(Value::Null))
                    .collect();
                c.topic = Some(if was_list {
                    Value::List(out)
                } else {
                    match out.len() {
                        1 => out.pop().expect("len checked"),
                        _ => Value::List(out),
                    }
                });
                c
            })
            .collect(),
        // The spread: fork a list topic into one thread per
        // element; null forks to nothing, a non-list scalar passes
        // through, a record passes whole. The outer form `...?`
        // differs in one case: where the plain spread would fork
        // to nothing, it emits ONE thread with a null topic — the
        // row survives its empty side (OPTIONAL MATCH).
        Stage::Spread { outer } => caps
            .into_iter()
            .flat_map(|c| {
                let mut values = match c.topic.clone() {
                    Some(Value::List(items)) => items,
                    Some(Value::Null) | None => Vec::new(),
                    Some(other) => vec![other],
                };
                if *outer && values.is_empty() {
                    values.push(Value::Null);
                }
                values.into_iter().map(move |v| Capsa {
                    node: c.node,
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    marks: c.marks.clone(),
                    bindings: c.bindings.clone(),
                    arrived: c.arrived.clone(),
                })
            })
            .collect(),
        // Push the topic onto the register — or, in a node context
        // (no topic), MARK the node: the shared push spelling is
        // context-typed. A scalar goes to the register, a node to
        // the mark store; the two never mix.
        Stage::Push(name) => caps
            .into_iter()
            .map(|mut c| {
                match (c.topic.clone(), name.clone()) {
                    (None, Some(label)) => c.marks.push(Mark {
                        name: label,
                        node: c.node,
                    }),
                    (topic, name) => c.register.push(Reg {
                        name,
                        value: topic.unwrap_or(Value::Null),
                    }),
                }
                c
            })
            .collect(),
        // Evaluate the sub-expression from each node, reduce to a
        // scalar, set it as the topic, and push it.
        Stage::Subcontext { name, body } => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                // The body sees this capsa as its enclosing scope:
                // `$$.name` / `$$_` / `$$ord` reach it (correlated
                // subqueries).
                let scope = Scope {
                    register: &c.register,
                    topic: c.topic.as_ref(),
                    ordinal: Some(i + 1),
                    captures: &c.captures,
                    marks: &c.marks,
                    bindings: &c.bindings,
                    edge: None,
                    arrived: &c.arrived,
                    peers,
                    outer,
                };
                let value = subcontext_scalar(body, adapter, c.node, trace, Some(&scope));
                c.register.push(Reg {
                    name: name.clone(),
                    value: value.clone(),
                });
                c.topic = Some(value);
                c
            })
            .collect(),
        // A value expression against the capsa's node becomes the
        // topic.
        Stage::Expr(expr) => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = Scope {
                    register: &c.register,
                    topic: c.topic.as_ref(),
                    ordinal: Some(i + 1),
                    captures: &c.captures,
                    marks: &c.marks,
                    bindings: &c.bindings,
                    edge: None,
                    arrived: &c.arrived,
                    peers,
                    outer,
                };
                let value = operand_scalar(adapter, c.node, expr, trace, scope);
                c.topic = Some(value);
                c
            })
            .collect(),
        // ... and a pushed one is a computed column.
        Stage::ExprPush { name, expr } => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = Scope {
                    register: &c.register,
                    topic: c.topic.as_ref(),
                    ordinal: Some(i + 1),
                    captures: &c.captures,
                    marks: &c.marks,
                    bindings: &c.bindings,
                    edge: None,
                    arrived: &c.arrived,
                    peers,
                    outer,
                };
                let value = operand_scalar(adapter, c.node, expr, trace, scope);
                c.register.push(Reg {
                    name: name.clone(),
                    value: value.clone(),
                });
                c.topic = Some(value);
                c
            })
            .collect(),
        // `@| ungroup` flattens groups back to their member capsae,
        // each inheriting the group's key registers. A memberless
        // capsa passes through unchanged.
        Stage::Agg(call) if call.name == "ungroup" => caps
            .into_iter()
            .flat_map(|c| {
                if c.members.is_empty() {
                    return vec![c];
                }
                let key_regs = c.register;
                c.members
                    .into_iter()
                    .map(|mut m| {
                        m.register.extend(key_regs.iter().cloned());
                        m
                    })
                    .collect()
            })
            .collect(),
        // `@| window(span[, key])` — each capsa gains its context
        // neighbors at ordinal offsets within the span as members
        // (partial near the edges), with the list of their effective
        // topics as its topic: the group shape, so the per-capsa
        // aggregate machinery rolls for free. With a partition key,
        // neighbors are the nearest capsae whose key compares equal,
        // original order preserved.
        Stage::Agg(call) if call.name == "window" => {
            window_stage(call, caps, adapter, trace, outer)
        }
        // `@| shift(n[, key])` — each capsa's topic becomes the
        // effective topic of the capsa `n` positions back (negative
        // looks forward; null where none exists). Registers and
        // captures stay the capsa's own.
        Stage::Agg(call) if call.name == "shift" => shift_stage(call, caps, adapter, trace, outer),
        // Keyed aggregates reorder or filter the capsae themselves,
        // preserving nodes, registers, and topics.
        Stage::Agg(call) if stdlib::known_keyed(&call.name) => {
            keyed_agg(call, caps, adapter, trace, outer)
        }
        // The order/selection family is capsa-preserving too: it
        // reorders or picks, never reduces, so nodes and registers
        // ride along (making `@| sort_by(…) @| reverse | [..n]`
        // compose). The effective topic is baked in, keeping value
        // outputs identical to a plain reduction's.
        Stage::Agg(call)
            if matches!(
                call.name.as_str(),
                "sort" | "unique" | "reverse" | "first" | "last"
            ) =>
        {
            let mut caps: Vec<Capsa> = caps
                .into_iter()
                .map(|mut c| {
                    c.topic = Some(
                        c.topic
                            .take()
                            .unwrap_or_else(|| node_scalar(adapter, c.node)),
                    );
                    c
                })
                .collect();
            let topic_of = |c: &Capsa| c.topic.clone().unwrap_or(Value::Null);
            match call.name.as_str() {
                // With a locale argument, topics sort by their text
                // under that locale's collation (colligo; codepoint
                // fallback when the feature is compiled out).
                "sort" => match stdlib::collator_for(call) {
                    Some(coll) => caps.sort_by(|a, b| {
                        coll.compare(&topic_of(a).to_string(), &topic_of(b).to_string())
                    }),
                    None => caps.sort_by(|a, b| topic_of(a).compare(&topic_of(b))),
                },
                "reverse" => caps.reverse(),
                "unique" => {
                    let mut seen: Vec<String> = Vec::new();
                    caps.retain(|c| {
                        let key = topic_of(c).to_string();
                        if seen.contains(&key) {
                            false
                        } else {
                            seen.push(key);
                            true
                        }
                    });
                }
                "first" => caps.truncate(1),
                "last" => {
                    let n = caps.len().saturating_sub(1);
                    caps.drain(..n);
                }
                _ => {}
            }
            caps
        }
        // Positional selection from the whole context, with the same
        // numbering as the navigation-side predicates.
        Stage::Select(pred @ (Predicate::Index(_) | Predicate::Range(_, _))) => {
            positional(caps, pred)
        }
        // An expression predicate in select position — reachable only
        // through an inline aggregate pipe (`(... @| [cond] ...)`),
        // which unlike the top-level `@|` does not reject it at parse
        // time — filters per capsa exactly as `| [cond]` does, never a
        // silent pass-through of every item.
        Stage::Select(Predicate::Expr(e)) => {
            apply_stage(&Stage::Filter(e.clone()), caps, adapter, trace, outer)
        }
        // Per-capsa filter: each capsa is kept iff the condition
        // holds against its node.
        Stage::Filter(cond) => caps
            .into_iter()
            .enumerate()
            .filter_map(|(i, mut c)| {
                let scope = Scope {
                    register: &c.register,
                    topic: c.topic.as_ref(),
                    ordinal: Some(i + 1),
                    captures: &c.captures,
                    marks: &c.marks,
                    bindings: &c.bindings,
                    edge: None,
                    arrived: &c.arrived,
                    peers,
                    outer,
                };
                // A capsa that carries a correlation witness is
                // tested UNDER it — the pipeline filter is the
                // WHERE clause, null-propagating over a null
                // witness slot (so `[not $*1::x]` is the anti-
                // join), never a re-search of the trace. Without
                // a witness, the existential runs as in
                // navigation position.
                let hit = if c.bindings.is_empty() {
                    exists_binding(adapter, c.node, &[cond], trace, &mut Vec::new(), scope)
                } else {
                    eval_pred_expr(adapter, c.node, cond, trace, &c.bindings, scope)
                };
                if !hit {
                    return None;
                }
                // A successful `=~` in the filter binds its groups
                // for later stages ($1 …); a filter without one
                // leaves earlier captures intact.
                let groups = extract_captures(cond, adapter, c.node, trace, scope);
                if let Some(groups) = groups {
                    c.captures = groups;
                }
                Some(c)
            })
            .collect(),
        // Reduce the whole context's topics to a new context.
        Stage::Agg(call) => {
            let topics: Vec<Value> = caps
                .iter()
                .map(|c| {
                    c.topic
                        .clone()
                        .unwrap_or_else(|| node_scalar(adapter, c.node))
                })
                .collect();
            let node = caps.first().map_or(adapter.root(), |c| c.node);
            stdlib::apply(call, topics)
                .into_iter()
                .map(|v| Capsa {
                    node,
                    register: Vec::new(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: Vec::new(),
                    marks: Vec::new(),
                    bindings: Vec::new(),
                    arrived: Vec::new(),
                })
                .collect()
        }
        // Recall a register value into the topic.
        Stage::Recall(r) => caps
            .into_iter()
            .map(|mut c| {
                c.topic = Some(recall(&c.register, r));
                c
            })
            .collect(),
    }
}

/// Apply a keyed aggregate: reorder or filter the capsae by keys
/// evaluated per capsa against its node, preserving nodes,
/// registers, and topics. Sorting is stable; a composite key
/// compares lexicographically; a null key sorts like empty text
/// (consistent with `Value::compare` everywhere else).
fn keyed_agg(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    // `top` / `bottom` carry their count as a leading literal.
    let (count, key_args): (Option<usize>, &[Arg]) = match call.args.split_first() {
        Some((Arg::Lit(Value::Int(n)), rest)) if matches!(call.name.as_str(), "top" | "bottom") => {
            (Some((*n).max(0) as usize), rest)
        }
        _ => (None, &call.args),
    };
    let key_of = |c: &Capsa, i: usize| -> Vec<Value> {
        key_args
            .iter()
            .filter_map(|a| match a {
                Arg::Expr(e) => Some(operand_scalar(
                    adapter,
                    c.node,
                    e,
                    trace,
                    Scope {
                        register: &c.register,
                        topic: c.topic.as_ref(),
                        ordinal: Some(i + 1),
                        captures: &c.captures,
                        marks: &c.marks,
                        bindings: &c.bindings,
                        edge: None,
                        arrived: &c.arrived,
                        peers: None,
                        outer,
                    },
                )),
                Arg::Lit(_) | Arg::Range(_, _) => None,
            })
            .collect()
    };
    let compare_keys = |a: &[Value], b: &[Value]| -> Ordering {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| x.compare(y))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    };

    // A missing key does not compete — the ordering counterpart of
    // the numeric aggregates skipping non-numeric values. Extremes
    // and top/bottom exclude null-keyed capsae; sort_by keeps them,
    // last; unique_by treats null as its own group.
    let has_null = |k: &[Value]| k.iter().any(|v| matches!(v, Value::Null));

    // `group` partitions instead of reordering: one capsa per
    // distinct key (first-appearance order; null keys grouped out,
    // pandas' dropna), topic = the list of member topics, the key
    // fields pushed as named regs for later recall ($.city).
    if call.name == "group" {
        // (key, key fields, members) per group.
        type Group = (Vec<Value>, Vec<(String, Value)>, Vec<Capsa>);
        let mut groups: Vec<Group> = Vec::new();
        for (i, c) in caps.into_iter().enumerate() {
            let scope = Scope {
                register: &c.register,
                topic: c.topic.as_ref(),
                ordinal: Some(i + 1),
                captures: &c.captures,
                marks: &c.marks,
                bindings: &c.bindings,
                edge: None,
                arrived: &c.arrived,
                peers: None,
                outer,
            };
            let fields = record_fields(&call.args, adapter, c.node, trace, scope);
            let key: Vec<Value> = fields.iter().map(|(_, v)| v.clone()).collect();
            if key.iter().any(|v| matches!(v, Value::Null)) {
                continue;
            }
            match groups.iter_mut().find(|(k, _, _)| {
                k.len() == key.len()
                    && k.iter()
                        .zip(&key)
                        .all(|(a, b)| a.compare(b) == Ordering::Equal)
            }) {
                Some((_, _, members)) => members.push(c),
                None => groups.push((key, fields, vec![c])),
            }
        }
        return groups
            .into_iter()
            .map(|(_, fields, members)| {
                let node = members[0].node;
                let register = fields
                    .into_iter()
                    .map(|(name, value)| Reg {
                        name: Some(name),
                        value,
                    })
                    .collect();
                let topics: Vec<Value> = members
                    .iter()
                    .map(|m| {
                        m.topic
                            .clone()
                            .unwrap_or_else(|| node_scalar(adapter, m.node))
                    })
                    .collect();
                Capsa {
                    node,
                    register,
                    topic: Some(Value::List(topics)),
                    members,
                    captures: Vec::new(),
                    marks: Vec::new(),
                    bindings: Vec::new(),
                    arrived: Vec::new(),
                }
            })
            .collect();
    }

    let mut keyed: Vec<(Vec<Value>, Capsa)> = caps
        .into_iter()
        .enumerate()
        .map(|(i, c)| (key_of(&c, i), c))
        .collect();
    match call.name.as_str() {
        "sort_by" => {
            keyed.sort_by(|(a, _), (b, _)| {
                has_null(a)
                    .cmp(&has_null(b))
                    .then_with(|| compare_keys(a, b))
            });
        }
        // Descending / ascending, stable, truncated to the count.
        "top" => {
            keyed.retain(|(k, _)| !has_null(k));
            keyed.sort_by(|(a, _), (b, _)| compare_keys(b, a));
            keyed.truncate(count.unwrap_or(0));
        }
        "bottom" => {
            keyed.retain(|(k, _)| !has_null(k));
            keyed.sort_by(|(a, _), (b, _)| compare_keys(a, b));
            keyed.truncate(count.unwrap_or(0));
        }
        // First capsa per distinct key, in original order.
        "unique_by" => {
            let mut seen: Vec<Vec<Value>> = Vec::new();
            keyed.retain(|(k, _)| {
                if seen.iter().any(|s| compare_keys(s, k) == Ordering::Equal) {
                    false
                } else {
                    seen.push(k.clone());
                    true
                }
            });
        }
        // Every capsa achieving the extreme key (ties included).
        "min_by" | "max_by" => {
            keyed.retain(|(k, _)| !has_null(k));
            let want = if call.name == "min_by" {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            let extreme = keyed
                .iter()
                .map(|(k, _)| k.clone())
                .reduce(|a, b| if compare_keys(&b, &a) == want { b } else { a });
            if let Some(extreme) = extreme {
                keyed.retain(|(k, _)| compare_keys(k, &extreme) == Ordering::Equal);
            }
        }
        _ => {}
    }
    keyed.into_iter().map(|(_, c)| c).collect()
}

/// A capsa's effective topic: the topic if set, else the node's
/// default scalar.
fn effective_topic(c: &Capsa, adapter: &impl AstAdapter) -> Value {
    c.topic
        .clone()
        .unwrap_or_else(|| node_scalar(adapter, c.node))
}

/// Partition the context for `window` / `shift`: for each capsa, the
/// ordered list of context indices it may reach and its own position
/// within that list. Without a key there is one list — the whole
/// context. With a key, capsae whose keys compare equal share a
/// list, in original context order; a null key matches nothing, so
/// its capsa stands alone (the ordering counterpart of "a missing
/// key does not compete").
fn peer_lists(
    caps: &[Capsa],
    key: Option<&Operand>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> (Vec<Vec<usize>>, Vec<(usize, usize)>) {
    let Some(key) = key else {
        let all: Vec<usize> = (0..caps.len()).collect();
        let of = (0..caps.len()).map(|i| (0, i)).collect();
        return (vec![all], of);
    };
    let mut lists: Vec<(Option<Value>, Vec<usize>)> = Vec::new();
    let mut of = Vec::with_capacity(caps.len());
    for (i, c) in caps.iter().enumerate() {
        let scope = Scope {
            register: &c.register,
            topic: c.topic.as_ref(),
            ordinal: Some(i + 1),
            captures: &c.captures,
            marks: &c.marks,
            bindings: &c.bindings,
            edge: None,
            arrived: &c.arrived,
            peers: None,
            outer,
        };
        let k = operand_scalar(adapter, c.node, key, trace, scope);
        let slot = if matches!(k, Value::Null) {
            None
        } else {
            lists
                .iter()
                .position(|(lk, _)| matches!(lk, Some(lk) if lk.compare(&k) == Ordering::Equal))
        };
        match slot {
            Some(li) => {
                lists[li].1.push(i);
                of.push((li, lists[li].1.len() - 1));
            }
            None => {
                let owner = if matches!(k, Value::Null) {
                    None
                } else {
                    Some(k)
                };
                lists.push((owner, vec![i]));
                of.push((lists.len() - 1, 0));
            }
        }
    }
    (lists.into_iter().map(|(_, l)| l).collect(), of)
}

/// The span and optional partition key of a `window` / `shift` call
/// (shapes validated at parse time). A count `window(n)` is the
/// trailing span `-(n-1)..0`.
fn window_args(call: &FnCall) -> (Option<i64>, Option<i64>, Option<&Operand>) {
    let key = match call.args.get(1) {
        Some(Arg::Expr(e)) => Some(e),
        _ => None,
    };
    match call.args.first() {
        Some(Arg::Range(a, b)) => (*a, *b, key),
        Some(Arg::Lit(Value::Int(n))) => (Some(-(n - 1)), Some(0), key),
        _ => (None, None, key),
    }
}

/// `@| window(span[, key])`: populate each capsa's members with its
/// reachable neighbors at offsets within the span (0 = self; an open
/// end runs to the partition edge; a short edge window holds what
/// exists), and set the list topic from their effective topics.
fn window_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    let (from, to, key) = window_args(call);
    let (lists, of) = peer_lists(&caps, key, adapter, trace, outer);
    let windows: Vec<Vec<usize>> = of
        .iter()
        .map(|&(li, p)| {
            let list = &lists[li];
            let p = p as i64;
            let lo = from.map_or(0, |a| (p + a).max(0)) as usize;
            let hi = to.map_or(list.len() as i64 - 1, |b| {
                (p + b).min(list.len() as i64 - 1)
            });
            if hi < lo as i64 {
                return Vec::new();
            }
            list[lo..=hi as usize].to_vec()
        })
        .collect();
    let members: Vec<Vec<Capsa>> = windows
        .iter()
        .map(|w| w.iter().map(|&j| caps[j].clone()).collect())
        .collect();
    caps.into_iter()
        .zip(members)
        .map(|(mut c, members)| {
            c.topic = Some(Value::List(
                members
                    .iter()
                    .map(|m| effective_topic(m, adapter))
                    .collect(),
            ));
            c.members = members;
            c
        })
        .collect()
}

/// `@| shift(n[, key])`: replace each capsa's topic with the
/// effective topic of the capsa `n` positions back in its partition
/// (negative `n` looks forward; null past the edge). Consumes any
/// members — they no longer correspond to the topic.
fn shift_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    let (n, key) = match call.args.split_first() {
        Some((Arg::Lit(Value::Int(n)), rest)) => (
            *n,
            match rest.first() {
                Some(Arg::Expr(e)) => Some(e),
                _ => None,
            },
        ),
        _ => (1, None),
    };
    let (lists, of) = peer_lists(&caps, key, adapter, trace, outer);
    let topics: Vec<Value> = of
        .iter()
        .map(|&(li, p)| {
            let list = &lists[li];
            let idx = p as i64 - n;
            if idx < 0 || idx >= list.len() as i64 {
                Value::Null
            } else {
                effective_topic(&caps[list[idx as usize]], adapter)
            }
        })
        .collect();
    caps.into_iter()
        .zip(topics)
        .map(|(mut c, t)| {
            c.topic = Some(t);
            c.members = Vec::new();
            c
        })
        .collect()
}

/// Build a `record(...)` value for one capsa: a literal-string
/// argument names the argument after it; a projection argument names
/// itself (validated at parse time).
fn build_record(
    call: &FnCall,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    scope: Scope<'_>,
) -> Value {
    Value::Record(record_fields(&call.args, adapter, node, trace, scope))
}

/// Evaluate record-convention arguments (a literal string names the
/// argument after it; a projection names itself) into named fields.
/// Shared by `record(...)` and `group(...)`'s keys.
fn record_fields(
    args: &[Arg],
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    scope: Scope<'_>,
) -> Vec<(String, Value)> {
    let mut fields: Vec<(String, Value)> = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg {
            Arg::Lit(Value::Str(name)) => {
                let value = match args.next() {
                    Some(Arg::Expr(e)) => operand_scalar_bound(adapter, node, e, trace, &[], scope),
                    Some(Arg::Lit(v)) => v.clone(),
                    Some(Arg::Range(_, _)) | None => Value::Null,
                };
                fields.push((name.clone(), value));
            }
            Arg::Expr(e) => {
                let name = crate::ast::auto_field_name(e)
                    .unwrap_or_default()
                    .to_string();
                fields.push((
                    name,
                    operand_scalar_bound(adapter, node, e, trace, &[], scope),
                ));
            }
            Arg::Lit(v) => {
                // Unreachable after parse-time validation; keep a
                // defensive unnamed field.
                fields.push((String::new(), v.clone()));
            }
            // A range argument never reaches the record convention.
            Arg::Range(_, _) => {}
        }
    }
    fields
}

/// Evaluate a subcontext body from `node` and reduce it to a scalar.
fn subcontext_scalar(
    body: &Query,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Value {
    match eval_query_outer(body, adapter, node, trace, outer) {
        QueryResult::Values(mut vs) => match vs.len() {
            0 => Value::Null,
            1 => vs.pop().unwrap(),
            _ => Value::List(vs),
        },
        QueryResult::Nodes(ns) => Value::Int(ns.len() as i64),
    }
}

/// A node's default scalar (its name), used when an aggregation runs
/// over an unprojected context.
fn node_scalar(adapter: &impl AstAdapter, node: NodeId) -> Value {
    adapter.name(node).map_or(Value::Null, Value::Str)
}

/// Evaluate a value expression against `node`, reduced to its first
/// value (null when it selects nothing).
fn operand_scalar(
    adapter: &impl AstAdapter,
    node: NodeId,
    expr: &Operand,
    trace: &Trace,
    scope: Scope<'_>,
) -> Value {
    operand_scalar_bound(adapter, node, expr, trace, &[], scope)
}

/// [`operand_scalar`] under a correlation binding.
fn operand_scalar_bound(
    adapter: &impl AstAdapter,
    node: NodeId,
    expr: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Value {
    eval_operand(adapter, node, expr, trace, bound, scope)
        .into_iter()
        .next()
        .unwrap_or(Value::Null)
}

/// Combine two values arithmetically (spec: Value Expressions and
/// Arithmetic). Operands combine via their numeric readings; a
/// missing reading yields null, and null propagates. Integer
/// results that overflow promote to floats; `div` is always a
/// float; division (of any flavor) by zero is null.
/// Temporal arithmetic (spec: The Temporal Fragment). A TYPED
/// temporal operand (Instant or Duration) activates it — two plain
/// numbers or strings never reach here, so the numeric fragment's
/// rules stand — and then the partner coerces through its temporal
/// reading, exactly as comparisons do: "2024-02-15" + (30 | days)
/// is an instant. Instant − Instant = Duration; Instant ± Duration
/// = Instant; Duration ± Duration = Duration; Duration × n and
/// Duration ÷ n scale. Anything else involving a temporal value is
/// null (propagating).
fn temporal_arith(op: ArithOp, left: &Value, right: &Value, scale: UnitScale) -> Option<Value> {
    // Coerce a TEXT partner facing a typed temporal operand,
    // through whichever temporal-family reading the text has —
    // the two grammars are disjoint: instant text is date-shaped
    // (the written offset kept for the result's display), span
    // text is P-prefixed or unit-suffixed (builtin units first,
    // then the mounted unit table's time units, e.g. `1jaj`).
    // Numbers never lift in arithmetic (epoch point or span of
    // seconds? — ambiguous).
    let lift = |v: &Value| -> Option<Value> {
        match v {
            Value::Instant { .. } | Value::Duration { .. } => Some(v.clone()),
            Value::Str(s) => crate::temporal::parse_iso(s)
                .map(|(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                })
                .or_else(|| {
                    crate::temporal::parse_span(s)
                        .or_else(|| crate::temporal::span_from_units(s, scale))
                        .map(|(secs, nanos)| Value::Duration { secs, nanos })
                }),
            _ => None,
        }
    };
    let typed = |v: &Value| matches!(v, Value::Instant { .. } | Value::Duration { .. });
    // Duration × n / ÷ n keep the number a NUMBER — handle before
    // lifting so a scaling factor is never read as an epoch.
    if !(matches!(op, ArithOp::Mul) && (typed(left) || typed(right))
        || matches!(op, ArithOp::Div) && typed(left))
    {
        match (typed(left), typed(right)) {
            (true, false) => {
                if let Some(l) = lift(right) {
                    return temporal_arith(op, left, &l, scale);
                }
            }
            (false, true) => {
                if let Some(l) = lift(left) {
                    return temporal_arith(op, &l, right, scale);
                }
            }
            _ => {}
        }
    }
    use Value::{Duration, Instant};
    // Checked throughout: `parse_span` admits spans up to i64::MAX
    // seconds, so an extreme instant/duration text must null-
    // propagate (None here → the caller returns null), never panic
    // under debug assertions or wrap silently in release —
    // arithmetic never aborts a query.
    let add = |s: i64, n: u32, ds: i64, dn: u32, sign: i64| -> Option<(i64, u32)> {
        let mut secs = s.checked_add(sign.checked_mul(ds)?)?;
        let total = n as i64 + sign * dn as i64;
        let mut nanos = total;
        if nanos < 0 {
            nanos += 1_000_000_000;
            secs = secs.checked_sub(1)?;
        } else if nanos >= 1_000_000_000 {
            nanos -= 1_000_000_000;
            secs = secs.checked_add(1)?;
        }
        Some((secs, nanos as u32))
    };
    match (left, right, op) {
        (
            Instant {
                secs: a, nanos: an, ..
            },
            Instant {
                secs: b, nanos: bn, ..
            },
            ArithOp::Sub,
        ) => {
            let Some((secs, nanos)) = add(*a, *an, *b, *bn, -1) else {
                return Some(Value::Null);
            };
            Some(Duration { secs, nanos })
        }
        (
            Instant {
                secs,
                nanos,
                offset_min,
            },
            Duration { secs: d, nanos: dn },
            ArithOp::Add | ArithOp::Sub,
        ) => {
            let sign = if matches!(op, ArithOp::Add) { 1 } else { -1 };
            let Some((secs, nanos)) = add(*secs, *nanos, *d, *dn, sign) else {
                return Some(Value::Null);
            };
            Some(Instant {
                secs,
                nanos,
                offset_min: *offset_min,
            })
        }
        (
            Duration { secs: d, nanos: dn },
            Instant {
                secs,
                nanos,
                offset_min,
            },
            ArithOp::Add,
        ) => {
            let Some((secs, nanos)) = add(*secs, *nanos, *d, *dn, 1) else {
                return Some(Value::Null);
            };
            Some(Instant {
                secs,
                nanos,
                offset_min: *offset_min,
            })
        }
        (
            Duration { secs: a, nanos: an },
            Duration { secs: b, nanos: bn },
            ArithOp::Add | ArithOp::Sub,
        ) => {
            let sign = if matches!(op, ArithOp::Add) { 1 } else { -1 };
            let Some((secs, nanos)) = add(*a, *an, *b, *bn, sign) else {
                return Some(Value::Null);
            };
            Some(Duration { secs, nanos })
        }
        (Duration { secs, nanos }, n, ArithOp::Mul)
        | (n, Duration { secs, nanos }, ArithOp::Mul) => {
            let k = n.numeric()?;
            let total = (*secs as f64 + *nanos as f64 / 1e9) * k;
            Some(Duration {
                secs: total.floor() as i64,
                nanos: ((total - total.floor()) * 1e9) as u32,
            })
        }
        (Duration { secs, nanos }, n, ArithOp::Div) => {
            let k = n.numeric()?;
            if k == 0.0 {
                return Some(Value::Null);
            }
            let total = (*secs as f64 + *nanos as f64 / 1e9) / k;
            Some(Duration {
                secs: total.floor() as i64,
                nanos: ((total - total.floor()) * 1e9) as u32,
            })
        }
        (Instant { .. } | Duration { .. }, _, _) | (_, Instant { .. } | Duration { .. }, _) => {
            Some(Value::Null)
        }
        _ => None,
    }
}

/// Quantital arithmetic (spec: The Quantital Fragment). A TYPED
/// quantity activates it: Q ± Q on the same base (the result a
/// base-displayed quantity); Q × n and Q ÷ n scale — the written
/// form scales along, keeping the display unit; a TEXT partner
/// lifts through the unital reading. Numbers never lift into ±
/// (the explicit road is `| convert(unit)`); a base mismatch or
/// any other combination is null, propagating.
fn quantital_arith(op: ArithOp, left: &Value, right: &Value, scale: UnitScale) -> Option<Value> {
    use Value::Quantity;
    let typed = |v: &Value| matches!(v, Quantity { .. });
    if !typed(left) && !typed(right) {
        return None;
    }
    let rescale = |q: &Value, k: f64| -> Value {
        let Quantity {
            value,
            base,
            written,
        } = q
        else {
            unreachable!("rescale is called on quantities only");
        };
        Quantity {
            value: value * k,
            base: base.clone(),
            written: written.as_ref().map(|(v, u)| (v * k, u.clone())),
        }
    };
    // Scaling factors stay numbers, as with durations.
    match op {
        ArithOp::Mul => {
            return match (typed(left), typed(right)) {
                (true, false) => right
                    .numeric()
                    .map(|k| rescale(left, k))
                    .or(Some(Value::Null)),
                (false, true) => left
                    .numeric()
                    .map(|k| rescale(right, k))
                    .or(Some(Value::Null)),
                _ => Some(Value::Null), // Q × Q: undefined (v1)
            };
        }
        ArithOp::Div if typed(left) && !typed(right) => {
            return match right.numeric() {
                Some(k) if k != 0.0 => Some(rescale(left, 1.0 / k)),
                _ => Some(Value::Null),
            };
        }
        _ => {}
    }
    let lift = |v: &Value| -> Option<(f64, String)> {
        match v {
            Quantity { value, base, .. } => Some((*value, base.clone())),
            Value::Str(s) => {
                crate::quantity::parse_unit_text_with(s, scale).map(|(bv, b, ..)| (bv, b))
            }
            _ => None,
        }
    };
    match op {
        ArithOp::Add | ArithOp::Sub => {
            let (Some((a, ba)), Some((b, bb))) = (lift(left), lift(right)) else {
                return Some(Value::Null);
            };
            if ba != bb {
                return Some(Value::Null);
            }
            let v = if matches!(op, ArithOp::Add) {
                a + b
            } else {
                a - b
            };
            Some(Value::Quantity {
                value: v,
                base: ba,
                written: None,
            })
        }
        _ => Some(Value::Null),
    }
}

fn arith(op: ArithOp, left: &Value, right: &Value, scale: UnitScale) -> Value {
    if let Some(v) = temporal_arith(op, left, right, scale) {
        return v;
    }
    if let Some(v) = quantital_arith(op, left, right, scale) {
        return v;
    }
    let (Some(l), Some(r)) = (left.numeric_reading(), right.numeric_reading()) else {
        return Value::Null;
    };
    if let (Value::Int(a), Value::Int(b)) = (&l, &r) {
        let (a, b) = (*a, *b);
        let promoted = |f: fn(f64, f64) -> f64| Value::Float(f(a as f64, b as f64));
        return match op {
            ArithOp::Add => a
                .checked_add(b)
                .map(Value::Int)
                .unwrap_or_else(|| promoted(|x, y| x + y)),
            ArithOp::Sub => a
                .checked_sub(b)
                .map(Value::Int)
                .unwrap_or_else(|| promoted(|x, y| x - y)),
            ArithOp::Mul => a
                .checked_mul(b)
                .map(Value::Int)
                .unwrap_or_else(|| promoted(|x, y| x * y)),
            ArithOp::Div => {
                if b == 0 {
                    Value::Null
                } else {
                    Value::Float(a as f64 / b as f64)
                }
            }
            ArithOp::IDiv => {
                if b == 0 {
                    Value::Null
                } else {
                    // The one overflow (i64::MIN idiv -1) promotes.
                    a.checked_div(b)
                        .map(Value::Int)
                        .unwrap_or_else(|| promoted(|x, y| (x / y).trunc()))
                }
            }
            ArithOp::Mod => {
                if b == 0 {
                    Value::Null
                } else {
                    // i64::MIN mod -1 is mathematically 0.
                    Value::Int(a.checked_rem(b).unwrap_or(0))
                }
            }
        };
    }
    let (Some(a), Some(b)) = (l.numeric(), r.numeric()) else {
        return Value::Null;
    };
    match op {
        ArithOp::Add => Value::Float(a + b),
        ArithOp::Sub => Value::Float(a - b),
        ArithOp::Mul => Value::Float(a * b),
        ArithOp::Div | ArithOp::IDiv | ArithOp::Mod if b == 0.0 => Value::Null,
        ArithOp::Div => Value::Float(a / b),
        ArithOp::IDiv => {
            let t = (a / b).trunc();
            if t >= i64::MIN as f64 && t <= i64::MAX as f64 {
                Value::Int(t as i64)
            } else {
                Value::Float(t)
            }
        }
        ArithOp::Mod => Value::Float(a % b),
    }
}

/// Look up a register reference.
fn recall(register: &[Reg], r: &RegRef) -> Value {
    match r {
        RegRef::Top => register
            .last()
            .map(|r| r.value.clone())
            .unwrap_or(Value::Null),
        RegRef::Index(n) => register
            .get(n.saturating_sub(1))
            .map(|r| r.value.clone())
            .unwrap_or(Value::Null),
        RegRef::Named(name) => register
            .iter()
            .rev()
            .find(|r| r.name.as_deref() == Some(name.as_str()))
            .map(|r| r.value.clone())
            .unwrap_or(Value::Null),
        RegRef::Whole => Value::List(register.iter().map(|r| r.value.clone()).collect()),
        // The named view: one field per name, in first-push order,
        // carrying the latest value pushed under that name — so a
        // repointed column keeps its place in the row. Unnamed
        // regulae are not part of the record.
        RegRef::Record => {
            let mut fields: Vec<(String, Value)> = Vec::new();
            for reg in register {
                if let Some(name) = &reg.name {
                    match fields.iter_mut().find(|(n, _)| n == name) {
                        Some((_, v)) => *v = reg.value.clone(),
                        None => fields.push((name.clone(), reg.value.clone())),
                    }
                }
            }
            Value::Record(fields)
        }
    }
}

/// A register rendered as a hashable dedup key: names plus value
/// text (Value carries floats, so it cannot be a key itself).
fn reg_key(register: &[Reg]) -> Vec<(Option<String>, String)> {
    register
        .iter()
        .map(|r| (r.name.clone(), r.value.to_string()))
        .collect()
}

/// Run the navigation path starting from `start`, per-path: each
/// result carries the register its expansion path's pushes built.
/// Paths without pattern pushes carry empty registers and dedup by
/// node — the plain semantics, unchanged.
fn navigate_paths(
    elems: &[PathElem],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    seed_marks: &[Mark],
) -> Vec<(NodeId, Vec<Reg>, Vec<Mark>, Vec<EdgeCtx>)> {
    let mut ctx: Vec<(NodeId, Vec<Reg>, Vec<Mark>, Vec<EdgeCtx>)> =
        vec![(start, Vec::new(), seed_marks.to_vec(), Vec::new())];
    for elem in elems {
        // A mark is not a hop: label the current node in place and
        // keep the crossing record intact.
        if let PathElem::Mark(name) = elem {
            for (node, _, marks, _) in &mut ctx {
                marks.push(Mark {
                    name: name.clone(),
                    node: *node,
                });
            }
            continue;
        }
        // Each element starts a fresh crossing record: `@-` is the
        // FINAL hop's edges, so only the last element's survive.
        let mut next: Vec<(NodeId, Vec<Reg>, Vec<Mark>, Vec<EdgeCtx>)> = Vec::new();
        for (node, register, marks, _) in &ctx {
            match elem {
                PathElem::Step(step) => {
                    let mut succs = Vec::new();
                    apply_step(adapter, *node, step, trace, outer, marks, &mut succs);
                    next.extend(succs.into_iter().map(|s| {
                        let arrived = arrived_edge(adapter, *node, step, s).into_iter().collect();
                        (s, register.clone(), marks.clone(), arrived)
                    }));
                }
                PathElem::Group(group) => {
                    let anchor = GPath {
                        node: *node,
                        visited: vec![*node],
                        register: register.clone(),
                        marks: marks.clone(),
                        last_edge: None,
                    };
                    next.extend(
                        expand_group(adapter, group, anchor, trace, outer)
                            .into_iter()
                            .map(|p| {
                                let arrived = p.last_edge.clone().into_iter().collect();
                                (p.node, p.register, p.marks, arrived)
                            }),
                    );
                }
                PathElem::Mark(_) => unreachable!("handled above"),
                // Pattern pushes live inside groups; the parser
                // enforces it.
                PathElem::Push { .. } => {}
            }
        }
        // Dedup by (node, register, marks), MERGING the crossings of
        // the collapsed duplicates — a node reached three ways keeps
        // one entry whose `@-` has three elements.
        let mut merged: Vec<(NodeId, Vec<Reg>, Vec<Mark>, Vec<EdgeCtx>)> = Vec::new();
        let mut index: HashMap<(NodeId, Vec<(Option<String>, String)>, Vec<(String, u64)>), usize> =
            HashMap::new();
        for (n, r, m, arrived) in next {
            match index.entry((n, reg_key(&r), mark_key(&m))) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    let slot = &mut merged[*e.get()].3;
                    for edge in arrived {
                        if !slot.contains(&edge) {
                            slot.push(edge);
                        }
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(merged.len());
                    merged.push((n, r, m, arrived));
                }
            }
        }
        ctx = merged;
        if ctx.is_empty() {
            break;
        }
    }
    ctx
}

/// The comparable view of a mark stack, for dedup keys.
fn mark_key(marks: &[Mark]) -> Vec<(String, u64)> {
    marks.iter().map(|m| (m.name.clone(), m.node.0)).collect()
}

/// The node-only view of [`navigate_paths`], for operand-position
/// paths: their pattern registers have no result capsa to land on
/// and are discarded.
fn navigate_from(
    elems: &[PathElem],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    seed_marks: &[Mark],
) -> Vec<NodeId> {
    dedup(
        navigate_paths(elems, adapter, start, trace, outer, seed_marks)
            .into_iter()
            .map(|(n, _, _, _)| n)
            .collect(),
    )
}

/// One in-flight expansion path through a path-pattern group: the
/// current node, every node the path has traversed (the group's
/// anchor included), the breadcrumbs its pushes have made, and the
/// edge it most recently walked (the `$-` a pattern stage sees).
/// Simple-path semantics: a hop whose target is already in `visited`
/// is not taken — which is what makes open quantifiers terminate on
/// cyclic crosslink data. `visited` is kept sorted (traversal order
/// carries no meaning for the cycle check), so two paths that cover
/// the same ground — and pushed the same breadcrumbs — compare equal
/// and dedup.
#[derive(Clone)]
struct GPath {
    node: NodeId,
    visited: Vec<NodeId>,
    register: Vec<Reg>,
    marks: Vec<Mark>,
    last_edge: Option<EdgeCtx>,
}

impl GPath {
    fn blocks(&self, node: NodeId) -> bool {
        self.visited.binary_search(&node).is_ok()
    }

    /// The path extended by one hop to `node`, along `edge`.
    fn with(&self, node: NodeId, edge: Option<EdgeCtx>) -> GPath {
        let mut visited = self.visited.clone();
        if let Err(i) = visited.binary_search(&node) {
            visited.insert(i, node);
        }
        GPath {
            node,
            visited,
            register: self.register.clone(),
            marks: self.marks.clone(),
            last_edge: edge,
        }
    }

    /// The dedup key: node, ground covered, breadcrumbs made, and
    /// the arrived-by edge (it feeds future pushes).
    fn key(
        &self,
    ) -> (
        NodeId,
        Vec<NodeId>,
        Vec<(Option<String>, String)>,
        Option<(NodeId, String, NodeId)>,
    ) {
        (
            self.node,
            self.visited.clone(),
            reg_key(&self.register),
            self.last_edge
                .as_ref()
                .map(|e| (e.source, e.label.clone(), e.target)),
        )
    }
}

/// The edge `step` walked from `from` to `succ` — the `$-` referent
/// a pattern stage sees. Crosslink labels are re-derived from the
/// adapter (first matching edge; the parallel-edge caveat of
/// `link_property` applies).
fn arrived_edge(
    adapter: &impl AstAdapter,
    from: NodeId,
    step: &Step,
    succ: NodeId,
) -> Option<EdgeCtx> {
    let edge = match &step.axis {
        Axis::Child | Axis::Descendant(_) => EdgeCtx {
            source: adapter.parent(succ).unwrap_or(from),
            label: CHILD.into(),
            target: succ,
        },
        Axis::Parent => EdgeCtx {
            source: from,
            label: PARENT.into(),
            target: succ,
        },
        Axis::Ancestor(_) => {
            // The chain's last hop: the child of `succ` on the walk.
            let mut child = from;
            while let Some(p) = adapter.parent(child) {
                if p == succ {
                    break;
                }
                child = p;
            }
            EdgeCtx {
                source: child,
                label: PARENT.into(),
                target: succ,
            }
        }
        Axis::NextSibling | Axis::FollowingSiblings(_) => EdgeCtx {
            source: from,
            label: NEXT.into(),
            target: succ,
        },
        Axis::PrevSibling | Axis::PrecedingSiblings(_) => EdgeCtx {
            source: from,
            label: PREV.into(),
            target: succ,
        },
        Axis::OutLink => {
            let label = adapter
                .links(from)
                .into_iter()
                .find(|(l, t)| *t == succ && matches_label(&step.matcher, l))
                .map(|(l, _)| l)?;
            EdgeCtx {
                source: from,
                label,
                target: succ,
            }
        }
        Axis::InLink => {
            let label = adapter
                .backlinks(from)
                .into_iter()
                .find(|(l, s)| *s == succ && matches_label(&step.matcher, l))
                .map(|(l, _)| l)?;
            EdgeCtx {
                source: succ,
                label,
                target: from,
            }
        }
        Axis::Resolve { property, .. } => EdgeCtx {
            source: from,
            label: property.clone(),
            target: succ,
        },
        Axis::ReverseResolve { property, .. } => EdgeCtx {
            source: succ,
            label: property.clone(),
            target: from,
        },
    };
    Some(edge)
}

/// Expand a path-pattern group from one anchor path. Repetitions run
/// from 1 to the effective bound (`min(n, N_max)` — the adapter's
/// quantifier bound caps open forms); each repetition is the union
/// over the group's alternatives; results collect at every count
/// `>= min`, with count 0 contributing the anchor itself when the
/// quantifier admits zero repetitions (`*`, `{0,n}`). The group's
/// reach then keeps all matches, or only those at the smallest (`?`)
/// or largest (`!`) repetition count.
fn expand_group(
    adapter: &impl AstAdapter,
    group: &Group,
    anchor: GPath,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<GPath> {
    let n_max = adapter.quantifier_bound();
    let hi = group.quant.max.map_or(n_max, |n| n.min(n_max));
    // The group's predicates filter matches BEFORE reach — the
    // walk continues from the full frontier, but only survivors
    // are candidates. `(...)+[P]?` is therefore "the nearest
    // satisfying P": a shortest-path search.
    let admits = |p: &GPath| group_admits(adapter, group, p, trace, outer);
    let mut matches: Vec<(GPath, usize)> = Vec::new();
    if group.quant.min == 0 && admits(&anchor) {
        matches.push((anchor.clone(), 0));
    }
    let mut frontier = vec![anchor];
    for count in 1..=hi {
        let mut next = Vec::new();
        for path in &frontier {
            for alt in &group.alts {
                next.extend(expand_elems(adapter, alt, vec![path.clone()], trace, outer));
            }
        }
        frontier = dedup_paths(next);
        if frontier.is_empty() {
            break;
        }
        if count >= group.quant.min {
            let before = matches.len();
            matches.extend(
                frontier
                    .iter()
                    .filter(|p| admits(p))
                    .map(|p| (p.clone(), count)),
            );
            // Proximal keeps only the smallest matched count: the
            // first tier with a survivor is the answer — stop
            // expanding.
            if group.reach == Reach::Proximal && matches.len() > before {
                break;
            }
        }
    }
    let extreme = match group.reach {
        Reach::All => None,
        Reach::Proximal => matches.iter().map(|&(_, c)| c).min(),
        Reach::Distal => matches.iter().map(|&(_, c)| c).max(),
    };
    matches
        .into_iter()
        .filter(|&(_, c)| extreme.is_none_or(|e| c == e))
        .map(|(p, _)| p)
        .collect()
}

/// Whether a path's endpoint passes the group's predicates, with
/// the path's arrived-by edge in scope as `$-`.
fn group_admits(
    adapter: &impl AstAdapter,
    group: &Group,
    path: &GPath,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> bool {
    if group.predicates.is_empty() {
        return true;
    }
    let exprs: Vec<&PredExpr> = group
        .predicates
        .iter()
        .filter_map(|p| match p {
            Predicate::Expr(e) => Some(e),
            _ => None,
        })
        .collect();
    exists_binding(
        adapter,
        path.node,
        &exprs,
        trace,
        &mut Vec::new(),
        Scope {
            outer,
            edge: path.last_edge.as_ref(),
            marks: &path.marks,
            ..NO_SCOPE
        },
    )
}

/// Fold one alternative's elements over a set of in-flight paths.
/// Steps apply from each path's node and drop successors the path has
/// already visited; nested groups recurse with the outer path's
/// visited set threaded through (a simple path never revisits a node,
/// however deeply the pattern nests).
fn expand_elems(
    adapter: &impl AstAdapter,
    elems: &[PathElem],
    mut paths: Vec<GPath>,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<GPath> {
    for elem in elems {
        // A mark labels each in-flight path's node in place.
        if let PathElem::Mark(name) = elem {
            for path in &mut paths {
                path.marks.push(Mark {
                    name: name.clone(),
                    node: path.node,
                });
            }
            continue;
        }
        let mut next = Vec::new();
        for path in &paths {
            match elem {
                PathElem::Step(step) => {
                    let mut succs = Vec::new();
                    apply_step(
                        adapter,
                        path.node,
                        step,
                        trace,
                        outer,
                        &path.marks,
                        &mut succs,
                    );
                    next.extend(succs.into_iter().filter(|&s| !path.blocks(s)).map(|s| {
                        let edge = arrived_edge(adapter, path.node, step, s);
                        path.with(s, edge)
                    }));
                }
                PathElem::Group(inner) => {
                    next.extend(expand_group(adapter, inner, path.clone(), trace, outer));
                }
                PathElem::Mark(_) => unreachable!("handled above"),
                // A breadcrumb: evaluate from the path's node — with
                // the walked edge as `$-` — and push. The path does
                // not move.
                PathElem::Push { name, body } => {
                    let scope = Scope {
                        edge: path.last_edge.as_ref(),
                        outer,
                        ..NO_SCOPE
                    };
                    let value = match body {
                        PushBody::Query(q) => {
                            subcontext_scalar(q, adapter, path.node, trace, Some(&scope))
                        }
                        PushBody::Expr(e) => {
                            operand_scalar_bound(adapter, path.node, e, trace, &[], scope)
                        }
                    };
                    let mut p = path.clone();
                    p.register.push(Reg {
                        name: name.clone(),
                        value,
                    });
                    next.push(p);
                }
            }
        }
        // Dedup by the full path key — never by node alone: two
        // paths at the same node with different histories (ground
        // covered, breadcrumbs, arrived-by edge) have different
        // futures.
        paths = dedup_paths(next);
        if paths.is_empty() {
            break;
        }
    }
    paths
}

/// Deduplicate in-flight paths while preserving first-seen order.
fn dedup_paths(paths: Vec<GPath>) -> Vec<GPath> {
    let mut seen = HashSet::new();
    paths.into_iter().filter(|p| seen.insert(p.key())).collect()
}

/// Project one node to a scalar value.
fn project(adapter: &impl AstAdapter, node: NodeId, proj: &Projection) -> Value {
    match proj {
        Projection::Property(Some(name)) => adapter.property(node, name).unwrap_or(Value::Null),
        Projection::Property(None) => adapter.default_value(node).unwrap_or(Value::Null),
        Projection::CoreMeta(key) => core_meta(adapter, node, key).unwrap_or(Value::Null),
        Projection::AdapterMeta(key) => adapter.metadata(node, key).unwrap_or(Value::Null),
    }
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
        "traits" => Some(Value::List(
            adapter.traits(node).into_iter().map(Value::Str).collect(),
        )),
        "is-leaf" => Some(Value::Bool(adapter.children(node).is_empty())),
        "is-root" => Some(Value::Bool(adapter.parent(node).is_none())),
        "is-first-child" => Some(Value::Bool(index() == Some(1))),
        "is-last-child" => Some(Value::Bool(
            matches!((index(), n_siblings()), (Some(i), Some(n)) if i == n + 1),
        )),
        "is-top-level" => Some(Value::Bool(depth_of(adapter, node) == 1)),
        // Non-tree nodes would carry depth -1; the current adapters put
        // every node in the child tree, so this is always false.
        "is-non-tree" => Some(Value::Bool(false)),
        // Root-relative location paths, one token per level.
        "name-path" => Some(Value::Str(ancestor_path(adapter, node, |a, n| a.name(n)))),
        "index-path" => Some(Value::Str(ancestor_path(adapter, node, |a, n| {
            index_of(a, n).map(|i| i.to_string())
        }))),
        "id-path" => Some(Value::Str(ancestor_path(adapter, node, |_, n| {
            Some(n.0.to_string())
        }))),
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
    adapter
        .children(parent)
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
    adapter
        .children(node)
        .into_iter()
        .map(|c| 1 + n_descendants(adapter, c))
        .sum()
}

/// Apply one step to a single source node, appending results to `out`.
#[allow(clippy::too_many_arguments)]
fn apply_step(
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    marks: &[Mark],
    out: &mut Vec<NodeId>,
) {
    match &step.axis {
        Axis::Child => {
            // Literal names take the adapter's name-addressed fast
            // path. The adapter owns the name test there (its
            // contract) — which permits deliberate aliasing, like
            // git revision syntax resolving to a hash-named commit
            // — so only the nameless tests re-run.
            let matched: Vec<NodeId> = match &step.matcher {
                Matcher::Name(n) => adapter
                    .children_named(node, n)
                    .into_iter()
                    .filter(|&c| tests_ok(adapter, c, step))
                    .collect(),
                _ => adapter
                    .children(node)
                    .into_iter()
                    .filter(|&c| matches_step(adapter, c, step))
                    .collect(),
            };
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |&n| n,
                |&n| {
                    Some(EdgeCtx {
                        source: node,
                        label: CHILD.into(),
                        target: n,
                    })
                },
            ));
        }
        Axis::Descendant(reach) => {
            let mut found = Vec::new();
            descendants(adapter, node, 1, step, &mut found);
            let found = apply_predicates(
                adapter,
                found,
                step,
                trace,
                outer,
                marks,
                |&(n, _)| n,
                // The arrived-by edge of a descendant match is its
                // own incoming tree edge.
                |&(n, _)| {
                    Some(EdgeCtx {
                        source: adapter.parent(n).unwrap_or(node),
                        label: CHILD.into(),
                        target: n,
                    })
                },
            );
            out.extend(by_reach(found, *reach));
        }
        Axis::Parent => {
            let matched: Vec<NodeId> = adapter
                .parent(node)
                .into_iter()
                .filter(|&p| matches_step(adapter, p, step))
                .collect();
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |&n| n,
                // Walked direction: from the child up.
                |&p| {
                    Some(EdgeCtx {
                        source: node,
                        label: PARENT.into(),
                        target: p,
                    })
                },
            ));
        }
        Axis::Ancestor(reach) => {
            // Collected nearest-first, so `[n]` indexes by proximity.
            // `preds` remembers each ancestor's predecessor on the
            // walked chain — the source of its arrived-by edge.
            let mut found = Vec::new();
            let mut preds: HashMap<NodeId, NodeId> = HashMap::new();
            let mut prev = node;
            let mut cur = adapter.parent(node);
            let mut dist = 1usize;
            while let Some(anc) = cur {
                if matches_step(adapter, anc, step) {
                    found.push((anc, dist));
                    preds.insert(anc, prev);
                }
                prev = anc;
                cur = adapter.parent(anc);
                dist += 1;
            }
            let found = apply_predicates(
                adapter,
                found,
                step,
                trace,
                outer,
                marks,
                |&(n, _)| n,
                |&(n, _)| {
                    Some(EdgeCtx {
                        source: preds.get(&n).copied().unwrap_or(node),
                        label: PARENT.into(),
                        target: n,
                    })
                },
            );
            out.extend(by_reach(found, *reach));
        }
        Axis::NextSibling => {
            let matched: Vec<NodeId> = sibling(adapter, node, 1)
                .into_iter()
                .filter(|&s| matches_step(adapter, s, step))
                .collect();
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |&n| n,
                |&sib| {
                    Some(EdgeCtx {
                        source: node,
                        label: NEXT.into(),
                        target: sib,
                    })
                },
            ));
        }
        Axis::PrevSibling => {
            let matched: Vec<NodeId> = sibling(adapter, node, -1)
                .into_iter()
                .filter(|&s| matches_step(adapter, s, step))
                .collect();
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |&n| n,
                |&sib| {
                    Some(EdgeCtx {
                        source: node,
                        label: PREV.into(),
                        target: sib,
                    })
                },
            ));
        }
        // Following/preceding siblings at any distance: matcher
        // first, then reach picks among the matches by distance
        // (nearest = adjacent side, farthest = the far end), in
        // document order.
        Axis::FollowingSiblings(reach) | Axis::PrecedingSiblings(reach) => {
            let following = matches!(step.axis, Axis::FollowingSiblings(_));
            let mut found: Vec<(NodeId, usize)> = Vec::new();
            if let Some(parent) = adapter.parent(node) {
                let sibs = adapter.children(parent);
                if let Some(pos) = sibs.iter().position(|&s| s == node) {
                    let range: Vec<NodeId> = if following {
                        sibs[pos + 1..].to_vec()
                    } else {
                        sibs[..pos].to_vec()
                    };
                    for (i, s) in range.iter().enumerate() {
                        if matches_step(adapter, *s, step) {
                            // Distance from the node, so `?` is the
                            // adjacent side for both directions.
                            let dist = if following { i + 1 } else { range.len() - i };
                            found.push((*s, dist));
                        }
                    }
                }
            }
            let found = apply_predicates(
                adapter,
                found,
                step,
                trace,
                outer,
                marks,
                |&(n, _)| n,
                |&(sib, _)| {
                    Some(EdgeCtx {
                        source: node,
                        label: if following { NEXT.into() } else { PREV.into() },
                        target: sib,
                    })
                },
            );
            out.extend(by_reach(found, *reach));
        }
        Axis::OutLink => {
            let matched: Vec<(String, NodeId)> = adapter
                .links(node)
                .into_iter()
                .filter(|(label, target)| {
                    matches_label(&step.matcher, label) && tests_ok(adapter, *target, step)
                })
                .collect();
            let kept = apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |(_, n)| *n,
                |(label, target)| {
                    Some(EdgeCtx {
                        source: node,
                        label: label.clone(),
                        target: *target,
                    })
                },
            );
            out.extend(kept.into_iter().map(|(_, target)| target));
        }
        Axis::InLink => {
            let matched: Vec<(String, NodeId)> = adapter
                .backlinks(node)
                .into_iter()
                .filter(|(label, source)| {
                    matches_label(&step.matcher, label) && tests_ok(adapter, *source, step)
                })
                .collect();
            let kept = apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |(_, n)| *n,
                // The walked edge points INTO the current node.
                |(label, source)| {
                    Some(EdgeCtx {
                        source: *source,
                        label: label.clone(),
                        target: node,
                    })
                },
            );
            out.extend(kept.into_iter().map(|(_, source)| source));
        }
        Axis::Resolve { property, hint } => {
            let matched: Vec<NodeId> = adapter
                .resolve(node, property, hint.as_deref())
                .into_iter()
                .filter(|&t| tests_ok(adapter, t, step))
                .collect();
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |&n| n,
                // A resolution edge is labeled by its property.
                |&t| {
                    Some(EdgeCtx {
                        source: node,
                        label: property.clone(),
                        target: t,
                    })
                },
            ));
        }
        Axis::ReverseResolve { property, hint } => {
            // "What points here?" — every node whose `property`
            // resolves to `node`. The naive scan walks the whole arbor
            // from the root; an adapter with a reverse index could
            // shortcut this later.
            let mut all = Vec::new();
            collect_subtree(adapter, adapter.root(), &mut all);
            let matched: Vec<NodeId> = all
                .into_iter()
                .filter(|&source| {
                    adapter.resolve(source, property, hint.as_deref()) == Some(node)
                        && tests_ok(adapter, source, step)
                })
                .collect();
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                marks,
                |&n| n,
                // Stored direction: the found node's property points
                // at the current one.
                |&source| {
                    Some(EdgeCtx {
                        source,
                        label: property.clone(),
                        target: node,
                    })
                },
            ));
        }
    }
}

/// Collect `node` and all of its descendants (the whole subtree).
fn collect_subtree(adapter: &impl AstAdapter, node: NodeId, out: &mut Vec<NodeId>) {
    out.push(node);
    for child in adapter.children(node) {
        collect_subtree(adapter, child, out);
    }
}

/// Collect proper descendants of `node` matching `step`'s tests (name,
/// traits, leaf — not predicates), in document order, tagged with
/// their depth (direct children are depth 1).
fn descendants(
    adapter: &impl AstAdapter,
    node: NodeId,
    depth: usize,
    step: &Step,
    out: &mut Vec<(NodeId, usize)>,
) {
    for child in adapter.children(node) {
        if matches_step(adapter, child, step) {
            out.push((child, depth));
        }
        descendants(adapter, child, depth + 1, step, out);
    }
}

/// Whether `node` passes `step`'s tests: the name matcher, the trait
/// filters, and the leaf anchor. Predicates are *not* checked here —
/// they apply to the hop's collected result list (`apply_predicates`).
fn matches_step(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    matches_name(adapter, node, &step.matcher) && tests_ok(adapter, node, step)
}

/// The nameless part of `matches_step` (trait filters and the leaf
/// anchor), for crosslink axes where the matcher applies to the edge
/// label instead of the node name.
fn tests_ok(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    if !traits_ok(adapter, node, step) {
        return false;
    }
    !step.leaf || adapter.children(node).is_empty()
}

/// Apply `step`'s predicates to one hop's result list (the tested
/// matches from a single source node, in axis order), sequentially:
///
/// - An index predicate `[n]` keeps the `n`-th element of the list
///   as filtered so far (`[-1]` the last) — position among the hop's
///   results, not sibling position (that is `[:::index = n]`). Out
///   of range keeps nothing.
/// - A range predicate `[a..b]` keeps the inclusive positional span,
///   under the same numbering; ends clamp to the list.
/// - A contiguous run of expression predicates is one conjunction
///   sharing a single correlation binding: a node is kept iff some
///   one tuple of trace-context nodes satisfies *every* `[...]` in
///   the run at once, so `$*k` means the same node across them.
///
/// A positional predicate therefore cuts the list between runs, and
/// is the one place where bracket order matters: `//user[::age >
/// 17][1]` is the first adult, `//user[1][::age > 17]` is the first
/// user if an adult.
fn apply_predicates<T>(
    adapter: &impl AstAdapter,
    mut items: Vec<T>,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    marks: &[Mark],
    node_of: impl Fn(&T) -> NodeId,
    edge_of: impl Fn(&T) -> Option<EdgeCtx>,
) -> Vec<T> {
    let mut i = 0;
    while i < step.predicates.len() {
        match &step.predicates[i] {
            pred @ (Predicate::Index(_) | Predicate::Range(_, _)) => {
                items = positional(items, pred);
                i += 1;
            }
            Predicate::Expr(_) => {
                let mut run: Vec<&PredExpr> = Vec::new();
                while let Some(Predicate::Expr(e)) = step.predicates.get(i) {
                    run.push(e);
                    i += 1;
                }
                items.retain(|item| {
                    let edge = edge_of(item);
                    exists_binding(
                        adapter,
                        node_of(item),
                        &run,
                        trace,
                        &mut Vec::new(),
                        // Navigation predicates have no capsa of
                        // their own; `$$…` still reaches the
                        // invoking subcontext's capsa. Edge hops
                        // define `$-` for their own predicates.
                        Scope {
                            outer,
                            edge: edge.as_ref(),
                            marks,
                            ..NO_SCOPE
                        },
                    )
                });
            }
        }
    }
    items
}

/// Apply a positional predicate (`[n]` / `[a..b]`) to a list: keep
/// the `n`-th item (1-based; negative from the end; out of range
/// keeps nothing), or the inclusive span (ends optional, negative
/// allowed, clamped to the list).
fn positional<T>(items: Vec<T>, pred: &Predicate) -> Vec<T> {
    match pred {
        Predicate::Index(n) => match resolve_pos(*n, items.len()) {
            Some(idx) => items.into_iter().nth(idx).into_iter().collect(),
            None => Vec::new(),
        },
        Predicate::Range(start, end) => {
            let len = items.len();
            let s = match start {
                None | Some(0) => 0,
                Some(v) if *v > 0 => (*v - 1) as usize,
                Some(v) => (len as i64 + *v).max(0) as usize,
            };
            // The exclusive upper bound of the inclusive end.
            let e = match end {
                None => len,
                Some(0) => 0,
                Some(v) if *v > 0 => (*v as usize).min(len),
                Some(v) => (len as i64 + *v + 1).max(0) as usize,
            };
            if s < e {
                items.into_iter().skip(s).take(e - s).collect()
            } else {
                Vec::new()
            }
        }
        // An expression predicate is not positional: both call sites
        // (the navigation-side `apply_predicates` and the `Stage::
        // Select` dispatch) route `Predicate::Expr` to per-capsa
        // filtering before it reaches here, so this arm is defensive —
        // it must never silently pass every item through as a filter.
        Predicate::Expr(_) => items,
    }
}

/// Resolve a 1-based, possibly negative position against a list of
/// `len` items. `None` when out of range (including 0).
fn resolve_pos(i: i64, len: usize) -> Option<usize> {
    if i > 0 && i as usize <= len {
        Some(i as usize - 1)
    } else if i < 0 && len as i64 + i >= 0 {
        Some((len as i64 + i) as usize)
    } else {
        None
    }
}

/// Search the product of the trace contexts for one tuple of bound
/// nodes under which *every* predicate in `exprs` holds. `bound`
/// accumulates one node per already-fixed trace context; a `$*k`
/// reference resolves to `bound[k-1]`. Returns as soon as a satisfying
/// tuple is found (existential), or false if none exists — including
/// when a trace context is empty, since then no tuple exists.
///
/// An *outer* context (`<=>?`) additionally offers a null binding,
/// tried only after every real candidate at its level failed. Under
/// a null binding, predicates that reference that context are
/// vacuous (a LEFT JOIN's ON clause never kills the driving row);
/// predicates that don't reference it still filter as usual.
fn exists_binding(
    adapter: &impl AstAdapter,
    node: NodeId,
    exprs: &[&PredExpr],
    trace: &Trace,
    bound: &mut Vec<Option<NodeId>>,
    scope: Scope<'_>,
) -> bool {
    if bound.len() == trace.contexts.len() {
        let hit = exprs.iter().all(|e| {
            mentions_null_ctx(e, bound) || eval_pred_expr(adapter, node, e, trace, bound, scope)
        });
        // Remember the admitting tuple — the *witness* — so the
        // pipeline's `$*k` operands can read the joined side back.
        if hit && !bound.is_empty() {
            trace
                .witnesses
                .borrow_mut()
                .entry(node.0)
                .or_insert_with(|| bound.clone());
        }
        return hit;
    }
    for &t in &trace.contexts[bound.len()] {
        bound.push(Some(t));
        let hit = exists_binding(adapter, node, exprs, trace, bound, scope);
        bound.pop();
        if hit {
            return true;
        }
    }
    if trace.outer.get(bound.len()).copied().unwrap_or(false) {
        bound.push(None);
        let hit = exists_binding(adapter, node, exprs, trace, bound, scope);
        bound.pop();
        if hit {
            return true;
        }
    }
    false
}

/// Whether a predicate expression references a trace context that is
/// currently bound to null — the exprs an outer join's null binding
/// makes vacuous.
fn mentions_null_ctx(e: &PredExpr, bound: &[Option<NodeId>]) -> bool {
    fn op_mentions(op: &Operand, bound: &[Option<NodeId>]) -> bool {
        match op {
            // A `$*k` reference bound to null — directly, or reached
            // through the context operand's own nested step predicates
            // (`$*1/sub[$*2::x]`).
            Operand::Ctx { index, steps, .. } => {
                matches!(index, Some(k) if matches!(bound.get(k.saturating_sub(1)), Some(None)))
                    || steps_mention(steps, bound)
            }
            // A relative path never references a context itself, but
            // its nested step predicates (`./sub[$*1::k = 1]`) can.
            Operand::Rel { steps, .. } => steps_mention(steps, bound),
            Operand::Neg(inner) | Operand::Outer(inner) => op_mentions(inner, bound),
            Operand::Arith { left, right, .. } => {
                op_mentions(left, bound) || op_mentions(right, bound)
            }
            Operand::Group(inner) => mentions_null_ctx(inner, bound),
            Operand::Piped { expr, .. } => op_mentions(expr, bound),
            Operand::Cond { cond, then, other } => {
                mentions_null_ctx(cond, bound)
                    || op_mentions(then, bound)
                    || op_mentions(other, bound)
            }
            // A value match references the context through its
            // scrutinee, any arm's test/result, or the else branch.
            Operand::Match {
                scrutinee,
                arms,
                other,
            } => {
                op_mentions(scrutinee, bound)
                    || arms.iter().any(|(test, _, result)| {
                        op_mentions(test, bound) || op_mentions(result, bound)
                    })
                    || op_mentions(other, bound)
            }
            Operand::Interp(segs) => segs.iter().any(|seg| match seg {
                InterpSeg::Expr(inner) => op_mentions(inner, bound),
                InterpSeg::Text(_) => false,
            }),
            _ => false,
        }
    }
    // Whether any `[...]` predicate nested in a path's steps
    // references the null-bound context.
    fn steps_mention(steps: &[PathElem], bound: &[Option<NodeId>]) -> bool {
        steps.iter().any(|elem| match elem {
            PathElem::Step(s) => preds_mention(&s.predicates, bound),
            PathElem::Group(g) => {
                preds_mention(&g.predicates, bound)
                    || g.alts.iter().any(|alt| steps_mention(alt, bound))
            }
            PathElem::Push { body, .. } => match body {
                PushBody::Expr(o) => op_mentions(o, bound),
                PushBody::Query(_) => false,
            },
            PathElem::Mark(_) => false,
        })
    }
    fn preds_mention(preds: &[Predicate], bound: &[Option<NodeId>]) -> bool {
        preds.iter().any(|p| match p {
            Predicate::Expr(e) => mentions_null_ctx(e, bound),
            Predicate::Index(_) | Predicate::Range(_, _) => false,
        })
    }
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            mentions_null_ctx(a, bound) || mentions_null_ctx(b, bound)
        }
        PredExpr::Not(a) => mentions_null_ctx(a, bound),
        PredExpr::Compare(l, _, r) => op_mentions(l, bound) || op_mentions(r, bound),
        PredExpr::Truthy(o) => op_mentions(o, bound),
    }
}

/// Whether a crosslink `label` satisfies `matcher`. `Any` matches any
/// label.
fn matches_label(matcher: &Matcher, label: &str) -> bool {
    matches!(matcher, Matcher::Any) || matcher.matches(label)
}

/// Whether `node` satisfies all of `step`'s `<...>` trait clauses.
fn traits_ok(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    if step.traits.is_empty() {
        return true;
    }
    let node_traits = adapter.traits(node);
    step.traits.iter().all(|c| c.matches(&node_traits))
}

/// Walk a filter condition for `=~` comparisons and return the
/// groups of the *last* successful match (Perl's rule), or `None`
/// when no `=~` matched. Non-participating groups capture as empty
/// text.
fn extract_captures(
    e: &PredExpr,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    scope: Scope<'_>,
) -> Option<Vec<String>> {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            let first = extract_captures(a, adapter, node, trace, scope);
            extract_captures(b, adapter, node, trace, scope).or(first)
        }
        PredExpr::Not(a) => extract_captures(a, adapter, node, trace, scope),
        PredExpr::Compare(l, CmpOp::Match, r) => {
            let pattern = operand_scalar_bound(adapter, node, r, trace, &[], scope);
            let re = Regex::new(&pattern.to_string()).ok()?;
            if re.captures_len() <= 1 {
                return None;
            }
            eval_operand(adapter, node, l, trace, &[], scope)
                .iter()
                .find_map(|v| {
                    let text = v.to_string();
                    re.captures(&text).map(|caps| {
                        caps.iter()
                            .skip(1)
                            .map(|g| g.map_or(String::new(), |m| m.as_str().to_string()))
                            .collect()
                    })
                })
        }
        PredExpr::Compare(..) | PredExpr::Truthy(_) => None,
    }
}

/// Evaluate a predicate expression under a fixed correlation binding
/// (`bound[k-1]` is the node that `$*k` resolves to). `trace` is still
/// threaded so that a nested navigation operand can carry it into its
/// own predicates.
fn eval_pred_expr(
    adapter: &impl AstAdapter,
    node: NodeId,
    e: &PredExpr,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> bool {
    match e {
        PredExpr::Or(a, b) => {
            eval_pred_expr(adapter, node, a, trace, bound, scope)
                || eval_pred_expr(adapter, node, b, trace, bound, scope)
        }
        PredExpr::And(a, b) => {
            eval_pred_expr(adapter, node, a, trace, bound, scope)
                && eval_pred_expr(adapter, node, b, trace, bound, scope)
        }
        PredExpr::Not(a) => !eval_pred_expr(adapter, node, a, trace, bound, scope),
        PredExpr::Compare(l, op, r) => {
            let lhs = eval_operand(adapter, node, l, trace, bound, scope);
            let rhs = eval_operand(adapter, node, r, trace, bound, scope);
            // Existential over a node's own multi-valued projections;
            // each `$*k` operand is a single bound node.
            lhs.iter().any(|a| {
                rhs.iter()
                    .any(|b| compare(a, *op, b, &|e| adapter.unit_scale(e)))
            })
        }
        PredExpr::Truthy(o) => eval_operand(adapter, node, o, trace, bound, scope)
            .iter()
            .any(Value::is_truthy),
    }
}

/// Evaluate an operand against `node` to a list of values (a
/// projection may yield several; a structural path yields one marker
/// per selected node).
fn eval_operand(
    adapter: &impl AstAdapter,
    node: NodeId,
    operand: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Vec<Value> {
    match operand {
        Operand::Lit(v) => vec![v.clone()],
        // The value match: scrutinee once, arms in order, first hit
        // wins, only the taken branch evaluates. A regex arm tests
        // like `=~`; a value arm compares by the standard equality.
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            let subject = operand_scalar_bound(adapter, node, scrutinee, trace, bound, scope);
            for (test, regex, result) in arms {
                let hit = if *regex {
                    let pat = operand_scalar_bound(adapter, node, test, trace, bound, scope);
                    regex_test(&subject, &pat, true)
                } else {
                    let t = operand_scalar_bound(adapter, node, test, trace, bound, scope);
                    value_eq(&subject, &t, &|e| adapter.unit_scale(e))
                };
                if hit {
                    return eval_operand(adapter, node, result, trace, bound, scope);
                }
            }
            eval_operand(adapter, node, other, trace, bound, scope)
        }
        // `now()` — the invocation instant, bound by the runner
        // before evaluation (never a clock read here); displayed
        // as UTC. Null when the runner bound none.
        Operand::Now => vec![
            adapter
                .invocation_instant()
                .map(|(secs, nanos)| Value::Instant {
                    secs,
                    nanos,
                    offset_min: Some(0),
                })
                .unwrap_or(Value::Null),
        ],
        // Capsa-scope operands: a register recall and the topic.
        Operand::Recall(r) => vec![recall(scope.register, r)],
        Operand::Topic => vec![scope.topic.cloned().unwrap_or(Value::Null)],
        Operand::Ordinal => vec![
            scope
                .ordinal
                .map(|i| Value::Int(i as i64))
                .unwrap_or(Value::Null),
        ],
        // `(cond ? then : else)` — only the taken branch
        // evaluates (an untaken branch's paths never navigate).
        Operand::Cond { cond, then, other } => {
            let taken = if eval_pred_expr(adapter, node, cond, trace, bound, scope) {
                then
            } else {
                other
            };
            eval_operand(adapter, node, taken, trace, bound, scope)
        }
        // `@*` — the stage's input context: bare, the peers'
        // topics; projected, the projection over their nodes. Null
        // where no context exists (navigation predicates).
        Operand::Capsae { projection } => {
            let Some(peers) = scope.peers else {
                return vec![Value::Null];
            };
            let vs: Vec<Value> = peers
                .iter()
                .map(|p| match projection {
                    None => p
                        .topic
                        .clone()
                        .unwrap_or_else(|| node_scalar(adapter, p.node)),
                    Some(proj) => project(adapter, p.node, proj),
                })
                .collect();
            vec![Value::List(vs)]
        }
        // `(expr | f @| g)` — the pipe tail: each value rides a
        // pseudo-capsa (this node and register, the value as topic)
        // through the ordinary stage machinery, so the semantics
        // mirror the pipeline by construction. `@|` sees a single
        // list value as a context of its elements.
        Operand::Piped { expr, stages } => {
            let mut state = eval_operand(adapter, node, expr, trace, bound, scope);
            for stage in stages.iter() {
                // A lone list value explodes into a context for the
                // aggregating forms.
                if matches!(stage, Stage::Agg(_) | Stage::Select(_))
                    && state.len() == 1
                    && matches!(state[0], Value::List(_))
                {
                    let Some(Value::List(items)) = state.pop() else {
                        unreachable!("matched above");
                    };
                    state = items;
                }
                let caps: Vec<Capsa> = state
                    .into_iter()
                    .map(|v| Capsa {
                        node,
                        register: scope.register.to_vec(),
                        topic: Some(v),
                        members: Vec::new(),
                        captures: scope.captures.to_vec(),
                        marks: scope.marks.to_vec(),
                        bindings: bound.to_vec(),
                        arrived: scope.arrived.to_vec(),
                    })
                    .collect();
                state = apply_stage(stage, caps, adapter, trace, Some(&scope))
                    .into_iter()
                    .map(|c| c.topic.unwrap_or(Value::Null))
                    .collect();
            }
            state
        }
        // `@-` — all final-hop crossings: labels bare, each edge's
        // property projected. Empty list where none.
        Operand::Edges { projection } => {
            let vs: Vec<Value> = scope
                .arrived
                .iter()
                .map(|edge| match projection {
                    None | Some(Projection::Property(None)) => Value::Str(edge.label.clone()),
                    Some(Projection::Property(Some(prop))) => adapter
                        .link_property(edge.source, &edge.label, edge.target, prop)
                        .unwrap_or(Value::Null),
                    Some(_) => Value::Null,
                })
                .collect();
            vec![Value::List(vs)]
        }
        // `$-` — the arrived-by edge: its label bare, an edge
        // property projected. Null where no edge is in scope.
        Operand::Edge { projection } => {
            let Some(edge) = scope.edge else {
                return vec![Value::Null];
            };
            let v = match projection {
                None | Some(Projection::Property(None)) => Value::Str(edge.label.clone()),
                Some(Projection::Property(Some(prop))) => adapter
                    .link_property(edge.source, &edge.label, edge.target, prop)
                    .unwrap_or(Value::Null),
                // Refused at parse time.
                Some(_) => Value::Null,
            };
            vec![v]
        }
        // Unreachable: parameters are substituted at expansion time.
        Operand::Param(_) => vec![Value::Null],
        // `$$…` — the same operand, one scope out: the invoking
        // capsa of the enclosing subcontext body. Null at the top
        // level, where no enclosing scope exists.
        Operand::Outer(inner) => match scope.outer {
            Some(o) => eval_operand(adapter, node, inner, trace, bound, *o),
            None => vec![Value::Null],
        },
        // An interpolated string: each hole contributes its first
        // value as text (null splices as empty).
        Operand::Interp(segs) => {
            let mut out = String::new();
            for seg in segs {
                match seg {
                    InterpSeg::Text(t) => out.push_str(t),
                    InterpSeg::Expr(e) => {
                        let v = operand_scalar_bound(adapter, node, e, trace, bound, scope);
                        out.push_str(&v.to_string());
                    }
                }
            }
            vec![Value::Str(out)]
        }
        Operand::Capture(n) => vec![
            scope
                .captures
                .get(n - 1)
                .map(|s| Value::Str(s.clone()))
                .unwrap_or(Value::Null),
        ],
        Operand::Rel {
            steps,
            projection,
            anchored,
            mark,
        } => {
            // An anchored operand (`^`) navigates from the root —
            // the branch anchor's rule, in operand position. A
            // `(name)` anchor navigates from the marked node (most
            // recent mark under that name); an unset mark yields
            // nothing.
            let from = if let Some(m) = mark {
                match scope.marks.iter().rev().find(|k| &k.name == m) {
                    Some(k) => k.node,
                    None => return Vec::new(),
                }
            } else if *anchored {
                adapter.root()
            } else {
                node
            };
            let nodes = navigate_from(steps, adapter, from, trace, scope.outer, scope.marks);
            match projection {
                Some(p) => nodes.iter().map(|&n| project(adapter, n, p)).collect(),
                None => vec![Value::Bool(true); nodes.len()],
            }
        }
        // A correlation context reference. The base is the single node
        // bound to the k-th trace context under the current tuple
        // (`$*k`), or the node being filtered (`$*`, no index). `steps`
        // then descend from that base before projecting, so a context
        // whose data lives one hop down (e.g. a JSON object's field) is
        // still reachable.
        // Arithmetic: each side contributes its first value; the
        // combination rules (numeric readings, null propagation,
        // overflow promotion) live in `arith`.
        Operand::Arith { op, left, right } => {
            let l = operand_scalar_bound(adapter, node, left, trace, bound, scope);
            let r = operand_scalar_bound(adapter, node, right, trace, bound, scope);
            vec![arith(*op, &l, &r, &|e| adapter.unit_scale(e))]
        }
        Operand::Neg(inner) => {
            let v = operand_scalar_bound(adapter, node, inner, trace, bound, scope);
            vec![match v.numeric_reading() {
                Some(Value::Int(n)) => n
                    .checked_neg()
                    .map(Value::Int)
                    .unwrap_or(Value::Float(-(n as f64))),
                Some(Value::Float(f)) => Value::Float(-f),
                _ => Value::Null,
            }]
        }
        // A boolean group in operand position is its truth value.
        Operand::Group(e) => vec![Value::Bool(eval_pred_expr(
            adapter, node, e, trace, bound, scope,
        ))],
        Operand::Ctx {
            index,
            steps,
            projection,
        } => {
            // In predicates `bound` carries the tuple under test;
            // in pipeline stages it is empty and the capsa's
            // correlation *witness* takes over, so `$*k` projects
            // the joined side into output.
            let bound = if bound.is_empty() {
                scope.bindings
            } else {
                bound
            };
            let base = match index {
                Some(k) => bound.get(k.saturating_sub(1)).copied(),
                None => Some(Some(node)),
            };
            let base = match base {
                // Out of range: no such context.
                None => return Vec::new(),
                // An outer context no tuple admitted: null.
                Some(None) => return vec![Value::Null],
                Some(Some(n)) => n,
            };
            let nodes = if steps.is_empty() {
                vec![base]
            } else {
                navigate_from(steps, adapter, base, trace, scope.outer, scope.marks)
            };
            match projection {
                Some(p) => nodes.iter().map(|&n| project(adapter, n, p)).collect(),
                None => vec![Value::Bool(true); nodes.len()],
            }
        }
    }
}

/// A unit-expression resolver for the unital reading's criterion
/// text — the adapter's [`crate::adapter::AstAdapter::unit_scale`],
/// threaded so custom units resolve where comparisons happen.
pub(crate) type UnitScale<'a> = &'a dyn Fn(&str) -> Option<(f64, String)>;

/// The durational reading, extended by the mounted unit table:
/// builtin span text first (`min`/`h`/`d` keep their engine
/// meaning), then a magnitude on a custom time unit (`5rep`).
fn durational_with(v: &Value, scale: UnitScale) -> Option<(i64, u32)> {
    v.durational_reading().or_else(|| match v {
        Value::Str(s) => crate::temporal::span_from_units(s, scale),
        _ => None,
    })
}

/// Compare two scalar values under `op`.
fn compare(a: &Value, op: CmpOp, b: &Value, scale: UnitScale) -> bool {
    match op {
        CmpOp::Eq => value_eq(a, b, scale),
        CmpOp::Ne => !value_eq(a, b, scale),
        CmpOp::Lt => value_cmp(a, b, scale) == Some(Ordering::Less),
        CmpOp::Le => matches!(
            value_cmp(a, b, scale),
            Some(Ordering::Less | Ordering::Equal)
        ),
        CmpOp::Gt => value_cmp(a, b, scale) == Some(Ordering::Greater),
        CmpOp::Ge => matches!(
            value_cmp(a, b, scale),
            Some(Ordering::Greater | Ordering::Equal)
        ),
        CmpOp::Match => regex_test(a, b, true),
        CmpOp::NotMatch => regex_test(a, b, false),
        // A null operand doesn't participate: null-propagate like
        // every other comparison rather than stringifying to "" —
        // otherwise a null right-hand side ("" is a substring of
        // everything) would vacuously match every value. An actual
        // empty string still matches, as it should.
        CmpOp::Contains => match (a, b) {
            (Value::Null, _) | (_, Value::Null) => false,
            _ => a.to_string().contains(&b.to_string()),
        },
    }
}

fn value_eq(a: &Value, b: &Value, scale: UnitScale) -> bool {
    // Timeline equality when either side is an instant, the other
    // coercing via its temporal reading (epoch ints, ISO text).
    if matches!(a, Value::Instant { .. }) || matches!(b, Value::Instant { .. }) {
        return match (a.temporal_reading(), b.temporal_reading()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        };
    }
    // Span equality when either side is a duration, the other
    // coercing via its durational reading (numbers as seconds,
    // span text like `30d` or `P5DT3H`) — extended by the mounted
    // unit table, so criteria may speak the document's own time
    // units (`5rep`), exactly as quantity criteria do.
    if matches!(a, Value::Duration { .. }) || matches!(b, Value::Duration { .. }) {
        return match (durational_with(a, scale), durational_with(b, scale)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        };
    }
    // Dimension equality when either side is a quantity, the other
    // coercing via its unital reading (unit text like `5km`; a bare
    // number as the partner's base). Base mismatch: not equal.
    if matches!(a, Value::Quantity { .. }) || matches!(b, Value::Quantity { .. }) {
        return match crate::value::quantital_pair_with(a, b, scale) {
            Some((x, y)) => x == y,
            None => false,
        };
    }
    if let (Some(x), Some(y)) = (a.numeric(), b.numeric()) {
        return x == y;
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

fn value_cmp(a: &Value, b: &Value, scale: UnitScale) -> Option<Ordering> {
    if matches!(a, Value::Instant { .. }) || matches!(b, Value::Instant { .. }) {
        return match (a.temporal_reading(), b.temporal_reading()) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Duration { .. }) || matches!(b, Value::Duration { .. }) {
        return match (durational_with(a, scale), durational_with(b, scale)) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Quantity { .. }) || matches!(b, Value::Quantity { .. }) {
        return crate::value::quantital_pair_with(a, b, scale).and_then(|(x, y)| x.partial_cmp(&y));
    }
    if let (Some(x), Some(y)) = (a.numeric(), b.numeric()) {
        return x.partial_cmp(&y);
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// Reconstruct a node's subtree as a JSON value: a leaf yields
/// its scalar; a node whose children are named `0..n-1` in order
/// yields an array; any other node an object keyed by child names.
/// Adapter-agnostic, so it round-trips a JSON document and gives a
/// sensible JSON view of any other tree.
fn node_to_json(adapter: &impl AstAdapter, node: NodeId) -> Value {
    let children = adapter.children(node);
    if children.is_empty() {
        return adapter.default_value(node).unwrap_or(Value::Null);
    }
    let names: Vec<Option<String>> = children.iter().map(|&c| adapter.name(c)).collect();
    let is_array = names
        .iter()
        .enumerate()
        .all(|(i, n)| n.as_deref() == Some(i.to_string().as_str()));
    if is_array {
        Value::List(children.iter().map(|&c| node_to_json(adapter, c)).collect())
    } else {
        Value::Record(
            children
                .iter()
                .zip(&names)
                .map(|(&c, n)| (n.clone().unwrap_or_default(), node_to_json(adapter, c)))
                .collect(),
        )
    }
}

/// Serialize a value as XML, wrapping it in `<tag>` when it needs
/// a name (a scalar, or a list's items). A record's `@name`
/// fields become attributes, `#text` its text, the rest nested
/// elements — the inverse of the decode convention.
fn value_to_xml(v: &Value, tag: &str) -> String {
    match v {
        Value::Record(fields) => {
            let mut attrs = String::new();
            let mut body = String::new();
            for (k, val) in fields {
                if let Some(a) = k.strip_prefix('@') {
                    attrs.push_str(&format!(" {a}=\"{}\"", xml_escape(&val.to_string())));
                } else if k == "#text" {
                    body.push_str(&xml_escape(&val.to_string()));
                } else if let Value::List(items) = val {
                    for it in items {
                        body.push_str(&value_to_xml(it, &xml_tag(k)));
                    }
                } else {
                    body.push_str(&value_to_xml(val, &xml_tag(k)));
                }
            }
            format!("<{tag}{attrs}>{body}</{tag}>")
        }
        Value::List(items) => items.iter().map(|it| value_to_xml(it, tag)).collect(),
        other => format!("<{tag}>{}</{tag}>", xml_escape(&other.to_string())),
    }
}

/// Serialize a node's subtree as XML: the node's name is the tag
/// (an unnamed root emits its children bare); children nest, a
/// leaf carries its scalar as text.
fn node_to_xml(adapter: &impl AstAdapter, node: NodeId) -> String {
    match adapter.name(node) {
        Some(tag) => node_to_xml_named(adapter, node, &xml_tag(&tag)),
        None => adapter
            .children(node)
            .iter()
            .map(|&c| {
                let t = adapter
                    .name(c)
                    .map(|n| xml_tag(&n))
                    .unwrap_or_else(|| "item".into());
                node_to_xml_named(adapter, c, &t)
            })
            .collect(),
    }
}

fn node_to_xml_named(adapter: &impl AstAdapter, node: NodeId, tag: &str) -> String {
    let children = adapter.children(node);
    if children.is_empty() {
        let body = adapter
            .default_value(node)
            .map(|v| xml_escape(&v.to_string()))
            .unwrap_or_default();
        format!("<{tag}>{body}</{tag}>")
    } else {
        let inner: String = children
            .iter()
            .map(|&c| {
                let t = adapter
                    .name(c)
                    .map(|n| xml_tag(&n))
                    .unwrap_or_else(|| "item".into());
                node_to_xml_named(adapter, c, &t)
            })
            .collect();
        format!("<{tag}>{inner}</{tag}>")
    }
}

/// A valid XML element name, or `item` when the source name is
/// not one (e.g. a JSON array index "0" — tags cannot start with
/// a digit).
fn xml_tag(name: &str) -> String {
    let mut chars = name.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    if ok {
        name.to_string()
    } else {
        "item".to_string()
    }
}

/// Escape the five XML predefined characters in text/attributes.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Run one shell command for the `sh(...)` stage: `cmd` under
/// `sh -c`, the topic's text on stdin, stdout (one trailing newline
/// trimmed) as the result. A failing spawn or exit status is `None`
/// (the stage nulls it); stderr is inherited so diagnostics stay
/// visible.
fn run_shell(cmd: &str, topic: Option<&Value>) -> Option<Value> {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(v) = topic
        && let Some(mut stdin) = child.stdin.take()
    {
        let text = v.to_string();
        // Feed stdin from a separate thread so the parent drains
        // stdout concurrently: a command that echoes its input
        // (cat, jq, sort, …) over a topic larger than the OS pipe
        // buffer would otherwise deadlock — parent blocked writing
        // stdin, child blocked writing stdout. The thread drops
        // stdin on completion, giving the child EOF.
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
            if !text.ends_with('\n') {
                let _ = stdin.write_all(b"\n");
            }
        });
    }
    // Any stdin handle not moved into the writer thread (the
    // topicless case) is dropped so the child still sees EOF.
    drop(child.stdin.take());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    Some(Value::Str(text))
}

/// Regex match: `a` (as text) against `b` (as pattern); `want` selects
/// match vs non-match. A bad pattern never matches.
fn regex_test(a: &Value, b: &Value, want: bool) -> bool {
    // A null subject or pattern doesn't participate — the match is
    // undefined, so null-propagate rather than stringifying to "":
    // a null pattern would otherwise compile to the empty regex and
    // match everything. `=~` (want = true) yields false, `!~`
    // (want = false) yields true, mirroring `=` / `!=` on null.
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return !want;
    }
    match Regex::new(&b.to_string()) {
        Ok(re) => re.is_match(&a.to_string()) == want,
        Err(_) => false,
    }
}

/// Whether `node`'s name satisfies `matcher`. `Any` and the pattern
/// dot match any node, even an unnamed one (e.g. the root); the other
/// matchers require a name.
fn matches_name(adapter: &impl AstAdapter, node: NodeId, matcher: &Matcher) -> bool {
    match matcher {
        Matcher::Any | Matcher::Dot => true,
        _ => adapter
            .name(node)
            .is_some_and(|name| matcher.matches(&name)),
    }
}

/// The sibling of `node` at the given offset (`+1` next, `-1` prev).
fn sibling(adapter: &impl AstAdapter, node: NodeId, offset: isize) -> Option<NodeId> {
    let parent = adapter.parent(node)?;
    let sibs = adapter.children(parent);
    let i = sibs.iter().position(|&s| s == node)? as isize + offset;
    if i < 0 {
        return None;
    }
    sibs.get(i as usize).copied()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;
    use std::collections::HashMap;

    #[test]
    fn quantital_coercion_and_arith() {
        let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::quantity::scale_expr;
        let q = |value: f64, base: &str, written: Option<(f64, &str)>| Value::Quantity {
            value,
            base: base.into(),
            written: written.map(|(v, u)| (v, u.to_string())),
        };
        let len = q(42000.0, "m", Some((42.0, "km")));
        // Coercion: unit text either side, numbers as the base.
        assert_eq!(
            value_cmp(&len, &Value::Str("5km".into()), sc),
            Some(Ordering::Greater)
        );
        assert!(value_eq(&len, &Value::Str("42km".into()), sc));
        assert!(value_eq(&len, &Value::Float(42000.0), sc));
        assert_eq!(
            value_cmp(&len, &Value::Str("30mi".into()), sc),
            Some(Ordering::Less)
        );
        // Dimension mismatch: the comparison fails rather than lies.
        let power = q(290.0, "kg*m^2/s^3", Some((290.0, "W")));
        assert_eq!(value_cmp(&power, &Value::Str("5km".into()), sc), None);
        assert!(!value_eq(&power, &Value::Str("5km".into()), sc));
        // kW text against a W-minted quantity — the motivating case.
        assert_eq!(
            value_cmp(&power, &Value::Str("0.2kW".into()), sc),
            Some(Ordering::Greater)
        );
        // Arithmetic: Q ± unit text, Q × n; the display forms.
        assert_eq!(len.to_string(), "42 km");
        let sum = arith(ArithOp::Add, &len, &Value::Str("500 m".into()), sc);
        assert_eq!(sum.to_string(), "42500 m");
        let doubled = arith(ArithOp::Mul, &len, &Value::Int(2), sc);
        assert_eq!(doubled.to_string(), "84 km");
        // Numbers never lift into ± (| convert is the explicit road).
        assert!(matches!(
            arith(ArithOp::Add, &len, &Value::Int(5), sc),
            Value::Null
        ));
    }

    #[test]
    fn durational_coercion_and_lift() {
        let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::quantity::scale_expr;
        let two_h = Value::Duration {
            secs: 7200,
            nanos: 0,
        };
        // Comparison coercion beside a typed Duration: span text
        // (either grammar) and numbers-as-seconds.
        assert_eq!(
            value_cmp(&two_h, &Value::Str("90min".into()), sc),
            Some(Ordering::Greater)
        );
        assert!(value_eq(&two_h, &Value::Str("PT2H".into()), sc));
        assert!(value_eq(&two_h, &Value::Int(7200), sc));
        // No durational reading: the comparison fails.
        assert_eq!(value_cmp(&two_h, &Value::Str("hello".into()), sc), None);
        assert!(!value_eq(&two_h, &Value::Str("hello".into()), sc));
        // The arithmetic text-lift: instant − span text = instant,
        // instant − instant text = duration (disjoint grammars).
        let (s, n, _) = crate::temporal::parse_iso("2026-07-12T09:00:00Z").unwrap();
        let now = Value::Instant {
            secs: s,
            nanos: n,
            offset_min: Some(0),
        };
        let earlier = arith(ArithOp::Sub, &now, &Value::Str("12h".into()), sc);
        assert_eq!(earlier.to_string(), "2026-07-11T21:00:00Z");
        let span = arith(ArithOp::Sub, &now, &Value::Str("2026-07-12".into()), sc);
        assert_eq!(span.to_string(), "PT9H");
        // Numbers never lift in arithmetic (epoch or seconds?).
        assert!(matches!(
            arith(ArithOp::Sub, &now, &Value::Int(300), sc),
            Value::Null
        ));
        // Duration ± span text stays a duration.
        let sum = arith(ArithOp::Add, &two_h, &Value::Str("30min".into()), sc);
        assert_eq!(sum.to_string(), "PT2H30M");
    }

    /// A tiny in-memory arbor for testing navigation without I/O.
    struct MockTree {
        names: Vec<Option<String>>,
        kids: HashMap<u64, Vec<u64>>,
        /// Outgoing crosslinks: node -> [(label, target)].
        links: HashMap<u64, Vec<(String, u64)>>,
        /// Edge properties: (source, label, target) -> [(name, value)].
        edge_props: HashMap<(u64, String, u64), Vec<(String, Value)>>,
    }

    impl MockTree {
        /// ```text
        /// (root) ├ a ├ x.rs
        ///        │   └ y.txt
        ///        └ b ├ z.rs
        ///            └ deep └ w.rs
        /// ```
        fn sample() -> Self {
            let names = vec![
                None,                 // 0 root
                Some("a".into()),     // 1
                Some("x.rs".into()),  // 2
                Some("y.txt".into()), // 3
                Some("b".into()),     // 4
                Some("z.rs".into()),  // 5
                Some("deep".into()),  // 6
                Some("w.rs".into()),  // 7
            ];
            let mut kids = HashMap::new();
            kids.insert(0, vec![1, 4]);
            kids.insert(1, vec![2, 3]);
            kids.insert(4, vec![5, 6]);
            kids.insert(6, vec![7]);
            // x.rs --ref--> w.rs
            let mut links = HashMap::new();
            links.insert(2, vec![("ref".to_string(), 7)]);
            MockTree {
                names,
                kids,
                links,
                edge_props: HashMap::new(),
            }
        }

        /// The sample tree with a `mgr` crosslink cycle laid over it:
        /// x.rs → z.rs → w.rs → x.rs.
        fn cyclic() -> Self {
            let mut t = MockTree::sample();
            t.links = HashMap::new();
            t.links.insert(2, vec![("mgr".to_string(), 5)]);
            t.links.insert(5, vec![("mgr".to_string(), 7)]);
            t.links.insert(7, vec![("mgr".to_string(), 2)]);
            t
        }

        /// ```text
        /// (root) └ div └ ul └ li └ ul └ li └ ul └ li
        /// ```
        /// Three levels of nesting for `(/ul/li)` repetition.
        fn lists() -> Self {
            let names = vec![
                None,
                Some("div".into()), // 1
                Some("ul".into()),  // 2
                Some("li".into()),  // 3
                Some("ul".into()),  // 4
                Some("li".into()),  // 5
                Some("ul".into()),  // 6
                Some("li".into()),  // 7
            ];
            let mut kids = HashMap::new();
            for (parent, child) in [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7)] {
                kids.insert(parent, vec![child]);
            }
            MockTree {
                names,
                kids,
                links: HashMap::new(),
                edge_props: HashMap::new(),
            }
        }

        /// The weighted tree with a second edge converging on
        /// deep: a -4-> x.rs -12-> deep, a -1-> y.txt -5-> deep.
        fn converging() -> Self {
            let mut t = MockTree::weighted();
            t.links.get_mut(&3).map(|v| v.push(("e".to_string(), 6)));
            t.links.insert(3, vec![("e".to_string(), 6)]);
            t.edge_props.insert(
                (3, "e".to_string(), 6),
                vec![("qty".to_string(), Value::Int(5))],
            );
            t
        }

        /// A crosslink diamond with a back edge:
        /// a → x.rs, a → y.txt; x.rs → deep, y.txt → deep;
        /// deep → x.rs. All edges labeled `e`.
        fn diamond() -> Self {
            let mut t = MockTree::sample();
            t.links = HashMap::new();
            t.links
                .insert(1, vec![("e".to_string(), 2), ("e".to_string(), 3)]);
            t.links.insert(2, vec![("e".to_string(), 6)]);
            t.links.insert(3, vec![("e".to_string(), 6)]);
            t.links.insert(6, vec![("e".to_string(), 2)]);
            t
        }
    }

    impl MockTree {
        /// The sample tree with weighted `e` edges laid over it:
        /// a -4-> x.rs, a -1-> y.txt; x.rs -12-> deep.
        fn weighted() -> Self {
            let mut t = MockTree::sample();
            t.links = HashMap::new();
            t.links
                .insert(1, vec![("e".to_string(), 2), ("e".to_string(), 3)]);
            t.links.insert(2, vec![("e".to_string(), 6)]);
            let mut w = |s: u64, t_: u64, q: i64| {
                t.edge_props.insert(
                    (s, "e".to_string(), t_),
                    vec![("qty".to_string(), Value::Int(q))],
                );
            };
            w(1, 2, 4);
            w(1, 3, 1);
            w(2, 6, 12);
            t
        }
    }

    impl AstAdapter for MockTree {
        fn root(&self) -> NodeId {
            NodeId(0)
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            self.kids
                .get(&node.0)
                .map(|v| v.iter().map(|&i| NodeId(i)).collect())
                .unwrap_or_default()
        }
        fn name(&self, node: NodeId) -> Option<String> {
            self.names[node.0 as usize].clone()
        }
        fn parent(&self, node: NodeId) -> Option<NodeId> {
            self.kids
                .iter()
                .find(|(_, v)| v.contains(&node.0))
                .map(|(&p, _)| NodeId(p))
        }
        fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
            self.links
                .get(&node.0)
                .map(|v| v.iter().map(|(l, t)| (l.clone(), NodeId(*t))).collect())
                .unwrap_or_default()
        }
        /// `.rs` files are `<code>`, other files `<text>`; the
        /// directories carry no traits (so `<!*>` finds them).
        fn traits(&self, node: NodeId) -> Vec<String> {
            match self.names[node.0 as usize].as_deref() {
                Some(n) if n.ends_with(".rs") => vec!["code".into(), "file".into()],
                Some(n) if n.contains('.') => vec!["text".into(), "file".into()],
                _ => Vec::new(),
            }
        }
        fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
            self.links
                .iter()
                .flat_map(|(&src, v)| {
                    v.iter()
                        .filter(|(_, t)| *t == node.0)
                        .map(move |(l, _)| (l.clone(), NodeId(src)))
                })
                .collect()
        }
        fn link_property(
            &self,
            source: NodeId,
            label: &str,
            target: NodeId,
            name: &str,
        ) -> Option<Value> {
            self.edge_props
                .get(&(source.0, label.to_string(), target.0))?
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    fn run(q: &str, t: &impl AstAdapter) -> Vec<u64> {
        match eval(&parse(&lex(q).unwrap()).unwrap(), t) {
            QueryResult::Nodes(ns) => ns.into_iter().map(|n| n.0).collect(),
            QueryResult::Values(_) => panic!("expected nodes, got values"),
        }
    }

    fn vals(q: &str, t: &MockTree) -> Vec<Value> {
        match eval(&parse(&lex(q).unwrap()).unwrap(), t) {
            QueryResult::Values(vs) => vs,
            QueryResult::Nodes(_) => panic!("expected values, got nodes"),
        }
    }

    #[test]
    fn child_and_descendant() {
        let t = MockTree::sample();
        assert_eq!(run("/a", &t), vec![1]);
        assert_eq!(run("/a/x.rs", &t), vec![2]);
        assert_eq!(run("//*.rs", &t), vec![2, 5, 7]);
    }

    #[test]
    fn parent_and_ancestor() {
        let t = MockTree::sample();
        assert_eq!(run("//w.rs\\deep", &t), vec![6]);
        // ancestors of w.rs named 'b' -> node 4
        assert_eq!(run("//w.rs\\\\b", &t), vec![4]);
    }

    #[test]
    fn proximal_and_distal_descendant() {
        let t = MockTree::sample();
        // .rs descendants of b: z.rs (depth 1), w.rs (depth 2)
        assert_eq!(run("/b//*.rs", &t), vec![5, 7]);
        assert_eq!(run("/b//?*.rs", &t), vec![5]); // nearest
        assert_eq!(run("/b//!*.rs", &t), vec![7]); // farthest
    }

    #[test]
    fn siblings() {
        let t = MockTree::sample();
        assert_eq!(run("/a/x.rs>y.txt", &t), vec![3]);
        assert_eq!(run("/a/y.txt<x.rs", &t), vec![2]);
        assert!(run("/a/y.txt>*", &t).is_empty()); // no next sibling
    }

    #[test]
    fn leaf_anchor() {
        let t = MockTree::sample();
        // 'deep' has a child, so it is not a leaf
        assert!(run("/b/deep$", &t).is_empty());
        assert_eq!(run("/b/z.rs$", &t), vec![5]);
    }

    #[test]
    fn regex_name() {
        let t = MockTree::sample();
        assert_eq!(run("//~(.*\\.rs)", &t), vec![2, 5, 7]);
    }

    #[test]
    fn correlation() {
        let t = MockTree::sample();
        // Nodes at the same depth as `a` (depth 1): a and b.
        assert_eq!(run("//a <=> //*[:::depth = $*1:::depth]", &t), vec![1, 4]);
        // All .rs files except the one `x.rs` names (join by name).
        assert_eq!(
            run("//x.rs <=> //*.rs[:::name != $*1:::name]", &t),
            vec![5, 7]
        );
        // Chained: nodes whose depth matches a AND name matches x.rs.
        assert_eq!(
            run("//a <=> //x.rs <=> //*[:::depth = $*1:::depth]", &t),
            vec![1, 4]
        );
    }

    #[test]
    fn crosslinks() {
        let t = MockTree::sample();
        // x.rs --ref--> w.rs
        assert_eq!(run("//x.rs->ref", &t), vec![7]);
        assert_eq!(run("//x.rs->*", &t), vec![7]);
        // wrong label -> nothing
        assert!(run("//x.rs->other", &t).is_empty());
        // incoming: w.rs is pointed at by x.rs
        assert_eq!(run("//w.rs<-ref", &t), vec![2]);
        // continue navigating from the crosslink target
        assert_eq!(run("//x.rs->ref\\deep", &t), vec![6]);
    }

    #[test]
    fn quantified_child_group() {
        let t = MockTree::lists();
        // li nodes at nesting depths 1..3 (nodes 3, 5, 7)
        assert_eq!(run("/div(/ul/li)+", &t), vec![3, 5, 7]);
        assert_eq!(run("/div(/ul/li)+?", &t), vec![3]); // shallowest
        assert_eq!(run("/div(/ul/li)+!", &t), vec![7]); // deepest
        assert_eq!(run("/div(/ul/li){2}", &t), vec![5]);
        assert_eq!(run("/div(/ul/li){1,2}", &t), vec![3, 5]);
        assert_eq!(run("/div(/ul/li){1,2}!", &t), vec![5]);
        // min above the tree's depth: nothing
        assert!(run("/div(/ul/li){4,}", &t).is_empty());
    }

    #[test]
    fn quantified_crosslinks_on_a_cycle() {
        // mgr cycle: x.rs (2) → z.rs (5) → w.rs (7) → x.rs (2)
        let t = MockTree::cyclic();
        // Simple paths: the anchor is visited, so the cycle cannot
        // close — the chain stops after two hops, bound or no bound.
        assert_eq!(run("//x.rs(->mgr)+", &t), vec![5, 7]);
        assert_eq!(run("//x.rs(->mgr)+?", &t), vec![5]);
        assert_eq!(run("//x.rs(->mgr)+!", &t), vec![7]);
        assert_eq!(run("//x.rs(->mgr){2}", &t), vec![7]);
        // `*` admits zero repetitions: the anchor itself joins.
        assert_eq!(run("//x.rs(->mgr)*", &t), vec![2, 5, 7]);
        // Zero-repetition proximal: the anchor is the shortest match.
        assert_eq!(run("//x.rs(->mgr)*?", &t), vec![2]);
        // Incoming links run the cycle backwards.
        assert_eq!(run("//x.rs(<-mgr)+", &t), vec![7, 5]);
    }

    #[test]
    fn group_alternation() {
        let t = MockTree::sample();
        // Strict form: each alternative spells its own nav-op.
        assert_eq!(run("(/a|/b)", &t), vec![1, 4]);
        // Tolerated form: the op before '(' distributes over names.
        assert_eq!(run("/a/(x.rs|y.txt)", &t), vec![2, 3]);
        // Subpath alternatives of different lengths.
        assert_eq!(run("/b/(z.rs|deep/w.rs)", &t), vec![5, 7]);
        // Nested group inside an alternative.
        assert_eq!(run("(/a/(x.rs|y.txt)|/b/z.rs)", &t), vec![2, 3, 5]);
        // Mixing strict and tolerated alternatives refuses.
        let toks = lex("/a/(x.rs|/y.txt)").unwrap();
        assert!(parse(&toks).is_err());
        // An empty alternative refuses.
        let toks = lex("(/a|)").unwrap();
        assert!(parse(&toks).is_err());
    }

    #[test]
    fn dot_wildcard_and_sugar() {
        let t = MockTree::sample();
        // `//` is expressible as a dot-wildcard pattern.
        assert_eq!(run("/b(/.)*/w.rs", &t), run("/b//w.rs", &t));
        // `(/.){2}` — grandchildren; `/{2}` is sugar for it.
        assert_eq!(run("(/.){2}", &t), vec![2, 3, 5, 6]);
        assert_eq!(run("/{2}", &t), run("(/.){2}", &t));
        // A named hop takes a brace quantifier directly.
        let lists = MockTree::lists();
        assert_eq!(run("/div/ul/li(/ul/li){1}", &lists), vec![5]);
        // Outside a pattern, `.` stays a literal name (no such node).
        assert!(run("/.", &t).is_empty());
    }

    #[test]
    fn zero_repetitions_keep_the_anchor() {
        let t = MockTree::sample();
        // b itself (0 reps) plus deep (1 rep).
        assert_eq!(run("/b(/deep)*", &t), vec![4, 6]);
        // {0,1} spells the same reach explicitly.
        assert_eq!(run("/b(/deep){0,1}", &t), vec![4, 6]);
    }

    #[test]
    fn quantifier_bound_clamps_open_forms() {
        /// The lists tree with a quantifier bound of 2.
        struct Bounded(MockTree);
        impl AstAdapter for Bounded {
            fn root(&self) -> NodeId {
                self.0.root()
            }
            fn children(&self, node: NodeId) -> Vec<NodeId> {
                self.0.children(node)
            }
            fn name(&self, node: NodeId) -> Option<String> {
                self.0.name(node)
            }
            fn parent(&self, node: NodeId) -> Option<NodeId> {
                self.0.parent(node)
            }
            fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
                self.0.links(node)
            }
            fn quantifier_bound(&self) -> usize {
                2
            }
        }
        let t = Bounded(MockTree::lists());
        // The third li (node 7) sits beyond the bound.
        assert_eq!(run("/div(/ul/li)+", &t), vec![3, 5]);
        // An explicit max also clamps: min(3, N_max) = 2.
        assert_eq!(run("/div(/ul/li){1,3}", &t), vec![3, 5]);
        // A min beyond the bound can never be reached.
        assert!(run("/div(/ul/li){3,}", &t).is_empty());
    }

    #[test]
    fn group_in_a_predicate_path() {
        let t = MockTree::sample();
        // x.rs has an outgoing ref chain; y.txt does not.
        assert_eq!(run("/a/*[(->ref)+]", &t), vec![2]);
        let cyc = MockTree::cyclic();
        // every node on the mgr cycle reaches something in two hops
        assert_eq!(run("//*[(->mgr){2}]", &cyc), vec![2, 5, 7]);
    }

    #[test]
    fn group_predicates_filter_before_reach() {
        // mgr cycle: x.rs (2) → z.rs (5) → w.rs (7) → 2
        let t = MockTree::cyclic();
        // nearest satisfying: w.rs is two hops out; without the
        // predicate, +? would stop at z.rs (tier one).
        assert_eq!(run("//x.rs(->mgr)+[:::name = w.rs]?", &t), vec![7]);
        assert_eq!(run("//x.rs(->mgr)+?", &t), vec![5]);
        // farthest satisfying
        assert_eq!(run("//x.rs(->mgr)+[:::name =~ ~(rs)]!", &t), vec![7]);
        // no survivor anywhere: empty, not an error
        assert!(run("//x.rs(->mgr)+[:::name = nope]?", &t).is_empty());
        // an unquantified group takes predicates too — endpoints of
        // different-length alternatives, filtered uniformly
        let s = MockTree::sample();
        assert_eq!(run("/b/(z.rs|deep/w.rs)[:::name = w.rs]", &s), vec![7]);
        // zero repetitions: the anchor itself must pass
        assert_eq!(run("/a(/x.rs)*[:::name = a]", &s), vec![1]);
        // the walked edge is in scope for the group's predicate
        let w = MockTree::weighted();
        assert_eq!(run("//a(->e)+[$-::qty = 12]", &w), vec![6]);
        // positional predicates refuse
        let toks = lex("/a(/b)+[1]").unwrap();
        assert!(parse(&toks).is_err());
    }

    #[test]
    fn arrived_edges_aggregate() {
        let t = MockTree::converging();
        // deep is reached by two crossings; one capsa, both edges.
        assert_eq!(
            vals("//a->e->e | .(@-::qty)", &t),
            vec![Value::List(vec![Value::Int(12), Value::Int(5)])]
        );
        // bare @- reads the labels
        assert_eq!(
            vals("//a->e->e | .(@-)", &t),
            vec![Value::List(vec![
                Value::Str("e".into()),
                Value::Str("e".into())
            ])]
        );
        // tree walks arrive by [child] edges
        assert_eq!(
            vals("/a | .(@-)", &t),
            vec![Value::List(vec![Value::Str("[child]".into())])]
        );
        // per-path capsae stay distinct when their breadcrumbs
        // differ — one final crossing each; identical breadcrumbs
        // merge, and their crossings aggregate
        assert_eq!(
            vals("//a(->e.($-::qty))->e | .(@-) @| count", &t),
            vec![Value::Int(2)]
        );
        assert_eq!(
            vals("//a(->e.(1))->e | .(@-::qty)", &t),
            vec![Value::List(vec![Value::Int(12), Value::Int(5)])]
        );
        // stage form: `| @-::prop` sets the topic directly, and
        // `each` forks it
        assert_eq!(
            vals("//a->e->e | @-::qty | ... @| sum", &t),
            vec![Value::Int(17)]
        );
    }

    #[test]
    fn context_accessor_and_inline_pipes() {
        let t = MockTree::sample();
        // Bare @* reads the peers' topics; projected, their nodes.
        assert_eq!(
            vals("//*.rs:::name | .all((@* @| join(', ')))", &t),
            vec![
                Value::Str("x.rs, z.rs, w.rs".into()),
                Value::Str("x.rs, z.rs, w.rs".into()),
                Value::Str("x.rs, z.rs, w.rs".into()),
            ]
        );
        // The snapshot rule: a filter's own predicate reads the
        // stage's INPUT (all three), not the shrinking output.
        assert_eq!(
            vals("//*.rs | [(@*:::name @| count) = 3] @| count", &t),
            vec![Value::Int(3)]
        );
        // Inline | mirrors per-capsa semantics (list reduction);
        // inline @| explodes a lone list into a context.
        assert_eq!(
            vals("/a/x.rs | .n(((@*:::name | count)))", &t),
            vec![Value::Int(1)]
        );
        // Capsa-preserving aggregates ride the inline pipe too.
        assert_eq!(
            vals(
                "//*.rs | .top((@*:::name @| sort @| [1..2] @| join('-')))",
                &t
            )[0],
            Value::Str("w.rs-x.rs".into())
        );
        // Outside any context (navigation predicates), @* is null.
        assert_eq!(run("/*[@* = null]", &t).len(), 2);
        // Pushes refuse inline.
        let toks = lex("/a | .x((@* | .bad))").unwrap();
        assert!(parse(&toks).is_err());
    }

    #[test]
    fn conditionals() {
        let t = MockTree::sample();
        // basic selection by truthiness
        assert_eq!(
            vals(
                "/a/* | rec(:::name, 'kind', (:::name =~ ~(rs) ? 'code' : 'text'))",
                &t
            ),
            vec![
                Value::Record(vec![
                    ("name".into(), Value::Str("x.rs".into())),
                    ("kind".into(), Value::Str("code".into())),
                ]),
                Value::Record(vec![
                    ("name".into(), Value::Str("y.txt".into())),
                    ("kind".into(), Value::Str("text".into())),
                ]),
            ]
        );
        // chained else — the multi-branch form, no inner parens
        assert_eq!(
            vals(
                "//w.rs | ((:::depth = 1 ? 'top' : :::depth = 2 ? 'mid' : 'deep'))",
                &t
            ),
            vec![Value::Str("deep".into())]
        );
        // only the taken branch evaluates: the untaken side would
        // be null-valued, not an error, and structural conditions
        // work in the condition
        assert_eq!(
            vals("/b | ((/deep ? 'has-deep' : 'flat'))", &t),
            vec![Value::Str("has-deep".into())]
        );
        // composes with pipe tails
        assert_eq!(
            vals("/a | (((1 = 1 ? 'yes' : 'no') | upper))", &t),
            vec![Value::Str("YES".into())]
        );
        // null condition is falsy
        assert_eq!(
            vals("/a | (($_ ? 'topic' : 'none'))", &t),
            vec![Value::Str("none".into())]
        );
    }

    #[test]
    fn trait_negation() {
        let t = MockTree::sample();
        // files that are not code
        assert_eq!(run("//*<file && !code>", &t), vec![3]);
        // traitless nodes (the directories)
        assert_eq!(run("/*<!*>", &t), vec![1, 4]);
        // negation inside a disjunction: text or non-file — all
        // but the .rs files
        assert_eq!(run("/a/*<text || !file>", &t), vec![3]);
        // parenthesized algebra, CNF underneath
        assert_eq!(run("//*<(code || text) && !text>", &t), vec![2, 5, 7]);
    }

    #[test]
    fn map_pipe() {
        // deep is reached by two weighted crossings: qty [12, 5].
        let t = MockTree::converging();
        // map: transform each element, capsae unchanged
        assert_eq!(
            vals("//a->e->e | .(@-::qty) $| ($_ * 10) | json", &t),
            vec![Value::Str("[120, 50]".into())]
        );
        // filter elements ($_ = the element): the comprehension WHERE
        assert_eq!(
            vals("//a->e->e | .(@-::qty) $| [$_ > 6] | json", &t),
            vec![Value::Str("[12]".into())]
        );
        // positional slice
        assert_eq!(
            vals("//a->e->e | .(@-::qty) $| [1..1] | json", &t),
            vec![Value::Str("[12]".into())]
        );
        // @* inside a map reads the ELEMENT context: self-normalize
        assert_eq!(
            vals(
                "//a->e->e | .(@-::qty) $| (($_ * 100 div (@* @| sum) | round)) | json",
                &t
            ),
            vec![Value::Str("[71, 29]".into())]
        );
        // null maps to null; a non-list degenerates to plain |
        assert_eq!(vals("/a | $_ $| upper", &t), vec![Value::Null]);
        assert_eq!(
            vals("/a | :::name $| upper", &t),
            vec![Value::Str("A".into())]
        );
        // pushes refuse inside a map
        let toks = lex("/a | @. $| .bad").unwrap();
        assert!(parse(&toks).is_err());
    }

    #[test]
    fn simple_path_dedup_is_by_visited_set() {
        // a → {x.rs, y.txt} → deep → x.rs (all labeled `e`): two
        // routes meet at deep with different histories; only the
        // y.txt route may continue back to x.rs. Deduping in-flight
        // paths by node alone would drop one history at deep and
        // could lose the count-3 match.
        let t = MockTree::diamond();
        assert_eq!(run("//a(->e)+", &t), vec![2, 3, 6]);
        // the longest simple path: a → y.txt → deep → x.rs
        assert_eq!(run("//a(->e)+!", &t), vec![2]);
    }

    #[test]
    fn edge_accessor() {
        let t = MockTree::weighted();
        // `$-::prop` filters BY the edge in the hop's own predicate:
        // a's heavy outgoing edges only.
        assert_eq!(run("//a->e[$-::qty > 1]", &t), vec![2]);
        // Bare `$-` reads the label.
        assert_eq!(run("//a->*[$- = e]", &t), vec![2, 3]);
        // Incoming direction: the walked edge points into the node.
        assert_eq!(run("//deep<-e[$-::qty = 12]", &t), vec![2]);
        // Null where no edge is in scope: nothing has walked one.
        assert!(run("/a[$-::qty = 4]", &t).is_empty());
        // Edge property in a projection-bearing filter stage after
        // the hop is NOT in scope (the edge belongs to the hop).
        assert_eq!(
            vals("//a->e[$-::qty > 1]:::name", &t),
            vec![Value::Str("x.rs".into())]
        );
    }

    #[test]
    fn pattern_breadcrumbs() {
        let t = MockTree::weighted();
        // Push the walked edge's property; read it back per path.
        // a -4-> x.rs -12-> deep: one path, register [4, 12].
        assert_eq!(
            vals("//a(->e.($-::qty))+ | [:::name = deep] | @. | product", &t),
            vec![Value::Int(48)]
        );
        // Bare $- pushes the label; tree hops push [child].
        assert_eq!(
            vals("(/a.($-)){1} | $.", &t),
            vec![Value::Str("[child]".into())]
        );
        // Depth as a value: count the breadcrumbs.
        assert_eq!(vals("//a(->e.(1))+! | @. | count", &t), vec![Value::Int(2)]);
        // A named push lands as a named regula. (Spaced: glued
        // `.q(` would lex into the matcher name, exactly as
        // `/x.rs(...)` keeps its filename.)
        assert_eq!(
            vals("//a(->e .q($-::qty)){1,2}! | $.q", &t),
            vec![Value::Int(12)]
        );
    }

    #[test]
    fn per_path_results() {
        // The diamond: two routes from a to deep, each its own
        // result capsa once the pattern pushes.
        let t = MockTree::diamond();
        assert_eq!(
            vals(
                "//a(->e.(:::name))+ | [:::name = deep] | @. | join(\"-\")",
                &t
            ),
            vec![
                Value::Str("x.rs-deep".into()),
                Value::Str("y.txt-deep".into()),
            ]
        );
        // Without pushes the same walk dedups by node, as before.
        assert_eq!(run("//a(->e)+ | [:::name = deep]", &t), vec![6]);
    }

    #[test]
    fn structural_edge_labels() {
        let t = MockTree::sample();
        // Every hop defines $-: tree, sibling, and reach walks.
        assert_eq!(run("/a[$- = '[child]']", &t), vec![1]);
        assert_eq!(run("//w.rs\\deep[$- = '[parent]']", &t), vec![6]);
        assert_eq!(run("/a/x.rs>y.txt[$- = '[next]']", &t), vec![3]);
        assert_eq!(run("/a/y.txt<x.rs[$- = '[prev]']", &t), vec![2]);
        // Before any hop, $- is null (nothing walked).
        assert!(run("/a[$- = '[parent]']", &t).is_empty());
    }

    #[test]
    fn pipelines_and_union() {
        let t = MockTree::sample();
        // count all .rs files (aggregation via @|)
        assert_eq!(vals("//*.rs @| count", &t), vec![Value::Int(3)]);
        // names, sorted and joined
        assert_eq!(
            vals("//*.rs:::name @| sort @| join(\", \")", &t),
            vec![Value::Str("w.rs, x.rs, z.rs".into())]
        );
        // per-capsa transform: uppercase each name
        assert_eq!(
            vals("//x.rs:::name | upper", &t),
            vec![Value::Str("X.RS".into())]
        );
        // union of two node sets: x.rs and z.rs
        assert_eq!(run("//x.rs || //z.rs", &t), vec![2, 5]);
        // aggregation applies to the whole union: count = 2
        assert_eq!(vals("//x.rs || //z.rs @| count", &t), vec![Value::Int(2)]);
    }

    #[test]
    fn registers_and_subcontexts() {
        let t = MockTree::sample();
        // subcontext: count of .rs descendants per top-level dir
        // a has x.rs (1 rs); b has z.rs and w.rs (2 rs)
        assert_eq!(
            vals("/*[:::name = a] | .(//*.rs @| count)", &t),
            vec![Value::Int(1)]
        );
        assert_eq!(
            vals("/*[:::name = b] | .(//*.rs @| count)", &t),
            vec![Value::Int(2)]
        );
        // push then recall a named breadcrumb
        assert_eq!(
            vals("//x.rs:::name | .saved | upper | $.saved", &t),
            vec![Value::Str("x.rs".into())]
        );
    }

    #[test]
    fn predicates() {
        let t = MockTree::sample();
        // dirs with exactly 2 children: a and b
        assert_eq!(run("/*[:::n-children = 2]", &t), vec![1, 4]);
        // structural condition: root children that have a z.rs child -> b
        assert_eq!(run("/*[/z.rs]", &t), vec![4]);
        // index predicate: first child of a -> x.rs
        assert_eq!(run("/a/*[1]", &t), vec![2]);
        // negation: non-leaf children of root -> a, b
        assert_eq!(run("/*[not :::is-leaf]", &t), vec![1, 4]);
        // and + regex match on the name
        assert_eq!(
            run("//*[:::is-leaf and :::name =~ ~(rs)]", &t),
            vec![2, 5, 7]
        );
        // comparison against a literal, via core depth
        assert_eq!(run("//*[:::depth > 2]", &t), vec![7]);
    }

    /// `[n]` selects by position among the hop's results (per source
    /// node, as filtered by the brackets to its left) — not by
    /// sibling position, which is `[:::index = n]`.
    #[test]
    fn index_predicate_positions_among_hop_results() {
        // (root) ├ h ├ p ├ p └ sect ├ title ├ p └ p
        let names = vec![
            None,                 // 0 root
            Some("h".into()),     // 1
            Some("p".into()),     // 2
            Some("p".into()),     // 3
            Some("sect".into()),  // 4
            Some("title".into()), // 5
            Some("p".into()),     // 6
            Some("p".into()),     // 7
        ];
        let mut kids = HashMap::new();
        kids.insert(0, vec![1, 2, 3, 4]);
        kids.insert(4, vec![5, 6, 7]);
        let t = MockTree {
            names,
            kids,
            links: HashMap::new(),
            edge_props: HashMap::new(),
        };
        // the first p child of the root — even though no p is the
        // root's first *child*
        assert_eq!(run("/p[1]", &t), vec![2]);
        assert_eq!(run("/p[2]", &t), vec![3]);
        assert_eq!(run("/sect/p[2]", &t), vec![7]);
        // a descendant hop yields one list per source node, so a
        // root-anchored //p[1] is the first p in the whole tree
        assert_eq!(run("//p[1]", &t), vec![2]);
        assert_eq!(run("//p[4]", &t), vec![7]);
        // sibling position is the metadata predicate
        assert_eq!(run("/p[:::index = 2]", &t), vec![2]);
        assert_eq!(run("/*[:::index = 3]", &t), vec![3]);
        assert_eq!(run("/p[:::index = 1]", &t), Vec::<u64>::new());
        // brackets apply left to right: filter-then-index differs
        // from index-then-filter
        assert_eq!(run("/*[:::index > 1][1]", &t), vec![2]);
        assert_eq!(run("/*[1][:::index > 1]", &t), Vec::<u64>::new());
        // negative indexes count from the end of the same list
        assert_eq!(run("/p[-1]", &t), vec![3]);
        assert_eq!(run("//p[-1]", &t), vec![7]);
        assert_eq!(run("//p[-4]", &t), vec![2]);
        assert_eq!(run("/*[:::index > 1][-1]", &t), vec![4]);
        // out of range (including 0) selects nothing
        assert_eq!(run("//p[0]", &t), Vec::<u64>::new());
        assert_eq!(run("//p[5]", &t), Vec::<u64>::new());
        assert_eq!(run("//p[-5]", &t), Vec::<u64>::new());
    }

    /// `[a..b]` keeps the inclusive positional span of the hop's
    /// results; either end may be omitted or negative, and ends
    /// clamp to the list.
    #[test]
    fn range_predicates() {
        let names = vec![
            None,                 // 0 root
            Some("h".into()),     // 1
            Some("p".into()),     // 2
            Some("p".into()),     // 3
            Some("sect".into()),  // 4
            Some("title".into()), // 5
            Some("p".into()),     // 6
            Some("p".into()),     // 7
        ];
        let mut kids = HashMap::new();
        kids.insert(0, vec![1, 2, 3, 4]);
        kids.insert(4, vec![5, 6, 7]);
        let t = MockTree {
            names,
            kids,
            links: HashMap::new(),
            edge_props: HashMap::new(),
        };
        // //p matches nodes 2, 3, 6, 7 in document order
        assert_eq!(run("//p[2..3]", &t), vec![3, 6]);
        assert_eq!(run("//p[2..]", &t), vec![3, 6, 7]);
        assert_eq!(run("//p[..2]", &t), vec![2, 3]);
        assert_eq!(run("//p[2..-1]", &t), vec![3, 6, 7]);
        assert_eq!(run("//p[..-2]", &t), vec![2, 3, 6]);
        // ends clamp; an empty span selects nothing
        assert_eq!(run("//p[..9]", &t), vec![2, 3, 6, 7]);
        assert_eq!(run("//p[5..]", &t), Vec::<u64>::new());
        assert_eq!(run("//p[3..2]", &t), Vec::<u64>::new());
        // ranges compose sequentially with filters
        assert_eq!(run("/*[..2][:::index > 1]", &t), vec![2]);
        assert_eq!(run("/*[:::index > 1][..2]", &t), vec![2, 3]);
    }

    /// Value expressions: arithmetic over numeric readings, with
    /// conventional precedence, null propagation, and exact integer
    /// arithmetic (spec: Value Expressions and Arithmetic).
    #[test]
    fn value_expressions() {
        let t = MockTree::sample();
        // literals, precedence, grouping (root children: a, b)
        assert_eq!(run("/*[1 + 2 * 3 = 7]", &t), vec![1, 4]);
        assert_eq!(run("/*[(1 + 2) * 3 = 9]", &t), vec![1, 4]);
        // metadata operands: a is the first child, b the second
        assert_eq!(run("/*[:::index + 1 = 2]", &t), vec![1]);
        assert_eq!(run("/*[:::index * 2 = 4]", &t), vec![4]);
        assert_eq!(run("/*[:::index - 2 = -1]", &t), vec![1]);
        assert_eq!(run("/*[- :::index = -2]", &t), vec![4]);
        // div is always a float; idiv truncates; mod follows the
        // dividend's sign
        assert_eq!(run("/*[7 div 2 = 3.5]", &t), vec![1, 4]);
        assert_eq!(run("/*[7 idiv 2 = 3]", &t), vec![1, 4]);
        assert_eq!(run("/*[7 mod 2 = 1]", &t), vec![1, 4]);
        assert_eq!(run("/*[0 - 7 mod 2 = -1]", &t), vec![1, 4]);
        // integer arithmetic is exact beyond float precision
        // (2^53 + 1 would round away under f64)
        assert_eq!(
            run("/*[9007199254740993 + 0 = 9007199254740993]", &t),
            vec![1, 4]
        );
        // division by zero and non-numeric operands are null, and
        // null is falsy
        assert_eq!(run("/*[1 div 0]", &t), Vec::<u64>::new());
        assert_eq!(run("/*[1 idiv 0]", &t), Vec::<u64>::new());
        assert_eq!(run("/*[:::name * 2]", &t), Vec::<u64>::new());
        // a glued minus stays a name character: no such property,
        // so the comparison selects nothing (but parses)
        assert_eq!(run("/*[::a-b = 1]", &t), Vec::<u64>::new());
        // boolean groups in operand position keep their old meaning
        assert_eq!(
            run("/*[(:::index = 1 or :::index = 2) and :::is-leaf]", &t),
            Vec::<u64>::new()
        );
    }

    /// Value expressions as pipeline stages: computed topics and
    /// computed (pushed) columns.
    #[test]
    fn value_expression_stages() {
        let t = MockTree::sample();
        // computed topic on one node: x.rs is child 1 of a
        assert_eq!(vals("/a/x.rs | :::index * 2", &t), vec![Value::Int(2)]);
        // a computed column pushed per capsa, then aggregated:
        // indexes 1 and 2 doubled sum to 6
        assert_eq!(
            vals("/* | .double(:::index * 2) @| sum", &t),
            vec![Value::Int(6)]
        );
        // grouped expression stage
        assert_eq!(
            vals("/a/x.rs | (:::index + 1) * 3", &t),
            vec![Value::Int(6)]
        );
    }

    /// Keyed aggregates reorder or filter capsae by per-capsa keys,
    /// preserving node identity so later stages keep working.
    #[test]
    fn keyed_aggregates() {
        let t = MockTree::sample();
        // sort leaves by name: w.rs, x.rs, y.txt, z.rs
        assert_eq!(
            vals("//*[:::is-leaf] @| sort_by(:::name) | :::name", &t),
            vec![
                Value::Str("w.rs".into()),
                Value::Str("x.rs".into()),
                Value::Str("y.txt".into()),
                Value::Str("z.rs".into()),
            ]
        );
        // descending via reverse; positional selection takes top-2
        assert_eq!(
            vals(
                "//*[:::is-leaf] @| sort_by(:::name) @| reverse @| [..2] | :::name",
                &t
            ),
            vec![Value::Str("z.rs".into()), Value::Str("y.txt".into())]
        );
        // ... which is what top does in one call
        assert_eq!(
            vals("//*[:::is-leaf] @| top(2, :::name) | :::name", &t),
            vec![Value::Str("z.rs".into()), Value::Str("y.txt".into())]
        );
        assert_eq!(
            vals("//*[:::is-leaf] @| bottom(1, :::name) | :::name", &t),
            vec![Value::Str("w.rs".into())]
        );
        // extreme-key selection keeps node identity (w.rs is node 7)
        assert_eq!(run("//*[:::is-leaf] @| max_by(:::depth)", &t), vec![7]);
        // ... and every tied capsa: both depth-1 nodes
        assert_eq!(run("//* @| min_by(:::depth)", &t), vec![1, 4]);
        // unique_by keeps the first capsa per distinct key
        assert_eq!(run("//* @| unique_by(:::depth)", &t), vec![1, 2, 7]);
        // composite keys tie-break
        assert_eq!(
            vals(
                "//*[:::is-leaf] @| sort_by(:::depth, :::name) | :::name",
                &t
            )[0],
            Value::Str("x.rs".into())
        );
    }

    /// Positional selection (`@| [n]`, whole-context) and per-capsa
    /// filters (`| [cond]`) as pipeline stages, each on its
    /// doctrinally correct pipe.
    #[test]
    fn positional_and_filter_stages() {
        let t = MockTree::sample();
        assert_eq!(run("//*.rs @| [1]", &t), vec![2]);
        assert_eq!(run("//*.rs @| [-1]", &t), vec![7]);
        assert_eq!(run("//*.rs @| [2..]", &t), vec![5, 7]);
        assert_eq!(
            vals("//*.rs @| [..2] | :::name", &t),
            vec![Value::Str("x.rs".into()), Value::Str("z.rs".into())]
        );
        // per-capsa filter: each capsa tested against its node
        assert_eq!(run("//* | [:::is-leaf]", &t), vec![2, 3, 5, 7]);
        assert_eq!(
            run("//*.rs @| sort_by(:::name) | [:::depth > 2]", &t),
            vec![7]
        );
        // the wrong pipe is a parse error pointing at the right one
        assert!(parse(&lex("//* | [1]").unwrap()).is_err());
        assert!(parse(&lex("//* @| [:::is-leaf]").unwrap()).is_err());
    }

    #[test]
    fn core_metadata_projection() {
        let t = MockTree::sample();
        assert_eq!(
            vals("/a/*:::name", &t),
            vec![Value::Str("x.rs".into()), Value::Str("y.txt".into())]
        );
        assert_eq!(vals("/a:::n-children", &t), vec![Value::Int(2)]);
        assert_eq!(vals("/b/deep:::depth", &t), vec![Value::Int(2)]);
        assert_eq!(vals("/a/x.rs:::is-leaf", &t), vec![Value::Bool(true)]);
        assert_eq!(vals("/b:::is-leaf", &t), vec![Value::Bool(false)]);
        assert_eq!(vals("/a/y.txt:::index", &t), vec![Value::Int(2)]);
        // parent metadata and level flags
        assert_eq!(vals("/a/x.rs:::parent-id", &t), vec![Value::Int(1)]);
        assert_eq!(vals("/a/x.rs:::parent-index", &t), vec![Value::Int(1)]);
        assert_eq!(vals("/a:::is-top-level", &t), vec![Value::Bool(true)]);
        assert_eq!(vals("/a/x.rs:::is-top-level", &t), vec![Value::Bool(false)]);
        // root-relative location paths (root contributes no segment)
        assert_eq!(
            vals("//w.rs:::name-path", &t),
            vec![Value::Str("/b/deep/w.rs".into())]
        );
        assert_eq!(
            vals("//w.rs:::index-path", &t),
            vec![Value::Str("/2/2/1".into())]
        );
        assert_eq!(
            vals("//w.rs:::id-path", &t),
            vec![Value::Str("/4/6/7".into())]
        );
    }

    #[test]
    fn ancestor_reach() {
        let t = MockTree::sample();
        // ancestors of w.rs that are directories: deep(1), b(2)
        assert_eq!(run("//w.rs\\\\*", &t), vec![6, 4, 0]);
        assert_eq!(run("//w.rs\\\\?*", &t), vec![6]); // nearest
        assert_eq!(run("//w.rs\\\\!*", &t), vec![0]); // farthest (root)
    }

    #[test]
    fn marks() {
        let t = MockTree::sample();
        // Mark mid-path; recall in a later step predicate — the
        // intra-path back-reference (Cypher's node variables).
        assert_eq!(run("/a .a /*[:::name = (a):::name]", &t), Vec::<u64>::new());
        assert_eq!(run("//deep .d /w.rs[(d):::name = \"deep\"]", &t), vec![7]);
        // Recall in a pipeline stage operand.
        assert_eq!(
            vals("//w.rs .w \\\\!* | (w):::name", &t),
            vec![Value::Str("w.rs".into())]
        );
        // Marks inside patterns: recall in a group predicate sees
        // the walk's own marks.
        assert_eq!(
            run("/a/x.rs .x (->ref)[(x):::name = \"x.rs\"]", &t),
            vec![7]
        );
        // The context-typed push: bare .name in a SCALAR context
        // still feeds the register, not the marks.
        assert_eq!(vals("//*.rs @| count | .n | $.n", &t), vec![Value::Int(3)]);
        // An unset mark yields nothing (predicate false), not an
        // error.
        assert_eq!(run("/a/*[(nope):::name = \"a\"]", &t), Vec::<u64>::new());
        // Marks seed sub-navigation: an anchored operand's NESTED
        // predicate still sees the thread's marks...
        assert_eq!(
            run("/a/x.rs .x | [^//w.rs[(x):::name = \"x.rs\"]]", &t),
            vec![2]
        );
        // ...and a subcontext body's branch can anchor on them.
        assert_eq!(
            vals("/a/x.rs .x | .n((x):::name) | $.n", &t),
            vec![Value::Str("x.rs".into())]
        );
    }

    #[test]
    fn shell_stage() {
        let t = MockTree::sample();
        // Gated: without allow_shell the run family refuses.
        let q = "/a/x.rs | :::name | `tr a-z A-Z`";
        assert!(crate::run(q, &t).is_err());
        // Allowed: topic through the command; failure nulls.
        struct Shelly(MockTree);
        impl AstAdapter for Shelly {
            fn root(&self) -> NodeId {
                self.0.root()
            }
            fn children(&self, n: NodeId) -> Vec<NodeId> {
                self.0.children(n)
            }
            fn name(&self, n: NodeId) -> Option<String> {
                self.0.name(n)
            }
            fn parent(&self, n: NodeId) -> Option<NodeId> {
                self.0.parent(n)
            }
            fn links(&self, n: NodeId) -> Vec<(String, NodeId)> {
                self.0.links(n)
            }
            fn allow_shell(&self) -> bool {
                true
            }
        }
        let t = Shelly(MockTree::sample());
        let got = match crate::run(q, &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert_eq!(got, vec![Value::Str("X.RS".into())]);
        let got = match crate::run("/a/x.rs | :::name | `false`", &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert_eq!(got, vec![Value::Null]);
    }

    #[test]
    fn node_mode_json() {
        let t = MockTree::sample();
        // Serialize a subtree: x.rs has no children (leaf) → its
        // scalar; a.rs's parent 'a' has children → object.
        let out = match crate::run("/a | json", &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert_eq!(out.len(), 1);
        assert!(out[0].to_string().contains("x.rs"));
        // decode(json): array → list, spread into rows.
        let out = match crate::run("^ | \"[1,2,3]\" | decode(json) | ...", &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert_eq!(out, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        // XML: | xml serializes a subtree; decode(xml) → value.
        let out = match crate::run("/a | xml", &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert!(out[0].to_string().starts_with("<a>") && out[0].to_string().contains("x.rs"));
        let out = match crate::run("^ | \"<r><x>1</x></r>\" | decode(xml) | json", &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert_eq!(out, vec![Value::Str("{\"x\": \"1\"}".into())]);
        // Object → record; round-trips through json.
        let out = match crate::run("^ | \"{\\\"a\\\":1}\" | decode(json) | json", &t).unwrap() {
            QueryResult::Values(vs) => vs,
            _ => panic!("values"),
        };
        assert_eq!(out, vec![Value::Str("{\"a\": 1}".into())]);
    }

    #[test]
    fn value_match() {
        let t = MockTree::sample();
        // Equality arms; scrutinee evaluated once; else required.
        assert_eq!(
            vals("/a/* | (:::name ?= \"x.rs\" ? 1 : \"y.txt\" ? 2 : 0)", &t),
            vec![Value::Int(1), Value::Int(2)]
        );
        // A regex arm tests =~ instead of equality.
        assert_eq!(
            vals(
                "//*.rs @| [..1] | (:::name ?= ~(^x) ? \"ex\" : \"other\")",
                &t
            ),
            vec![Value::Str("ex".into())]
        );
        // No arm hits: the else.
        assert_eq!(
            vals("/a/*[1] | (:::name ?= \"nope\" ? 1 : 9)", &t),
            vec![Value::Int(9)]
        );
    }

    #[test]
    fn anchored_operands() {
        let t = MockTree::sample();
        // Same-name-as-some-node-under-/a: compares existentially
        // against a set gathered from the root, not the candidate.
        assert_eq!(run("//*[:::name = ^/a/*:::name]", &t), vec![2, 3]);
        // Structural: keep nodes only if the arbor has a w.rs
        // anywhere — true from every candidate.
        assert_eq!(run("/a/*[^//w.rs]", &t), vec![2, 3]);
        // Inline pipe with an anchored head: the whole-tree count,
        // readable from any capsa.
        assert_eq!(
            vals("/a/*[1] | (^//*.rs @| count)", &t),
            vec![Value::Int(3)]
        );
    }

    #[test]
    fn outer_spread() {
        let t = MockTree::sample();
        // x.rs has a ref (to w.rs); y.txt has none. The plain
        // spread drops the refless thread; the outer spread keeps
        // it with a null topic.
        assert_eq!(
            vals("/a/* | .m(->ref:::name) | $.m | ...", &t),
            vec![Value::Str("w.rs".into())]
        );
        assert_eq!(
            vals("/a/* | .m(->ref:::name) | $.m | ...?", &t),
            vec![Value::Str("w.rs".into()), Value::Null]
        );
    }

    #[test]
    fn outer_correlation() {
        let t = MockTree::sample();
        // Left context: /a's children (x.rs, y.txt). Rows: every
        // .rs file. Only x.rs finds a same-named partner; the
        // outer marker keeps the other rows with a null witness.
        let q = "/a/* <=> ^//*.rs[:::name = $*1:::name]";
        assert_eq!(run(q, &t), vec![2]);
        let q = "/a/* <=>? ^//*.rs[:::name = $*1:::name]";
        assert_eq!(run(q, &t), vec![2, 5, 7]);
        // The witness reads back: the partner's name, or null.
        assert_eq!(
            vals("/a/* <=>? ^//*.rs[:::name = $*1:::name] | $*1:::name", &t),
            vec![Value::Str("x.rs".into()), Value::Null, Value::Null]
        );
        // The anti-join: a pipeline filter is the WHERE clause,
        // evaluated under the capsa's witness (null-propagating),
        // so testing the null slot keeps only the unmatched rows.
        let q = "/a/* <=>? ^//*.rs[:::name = $*1:::name] | [not $*1:::name]";
        assert_eq!(run(q, &t), vec![5, 7]);
        // And the matched-only filter is its complement.
        let q = "/a/* <=>? ^//*.rs[:::name = $*1:::name] | [$*1:::name]";
        assert_eq!(run(q, &t), vec![2]);
    }

    #[test]
    fn outer_correlation_indirect_ctx() {
        let t = MockTree::sample();
        // A `$*k` reference reached *indirectly* — through a value
        // match's arm, or a nested step predicate — must still be
        // made vacuous under the outer join's null binding, exactly
        // as a bare `$*1:::name` is (see `outer_correlation`).
        // Otherwise the unmatched rows (z.rs, w.rs) are wrongly
        // dropped, so semantically equivalent spellings diverge.
        let q = "/a/* <=>? ^//*.rs[(:::name ?= $*1:::name ? 1 : 0) = 1]";
        assert_eq!(run(q, &t), vec![2, 5, 7]);
        // The same reference, this time buried in a nested step
        // predicate (`$*1` inside the `/*` step's `[...]`) rather
        // than a value match. The `.rs` rows are leaves, so no real
        // binding admits any of them; only the null binding — made
        // vacuous once the step predicate is walked — keeps them.
        let q = "/a/* <=>? ^//*.rs[/*[$*1:::name = :::name]]";
        assert_eq!(run(q, &t), vec![2, 5, 7]);
    }

    #[test]
    fn now_operand() {
        let t = MockTree::sample();
        // The default (library) adapter binds no invocation instant,
        // so now() reads as null.
        assert_eq!(vals("^ | (now())", &t), vec![Value::Null]);

        // A runner that pins the instant: now() denotes it, displayed
        // as UTC (offset_min = Some(0)).
        struct Clocked(MockTree, i64, u32);
        impl AstAdapter for Clocked {
            fn root(&self) -> NodeId {
                self.0.root()
            }
            fn children(&self, n: NodeId) -> Vec<NodeId> {
                self.0.children(n)
            }
            fn name(&self, n: NodeId) -> Option<String> {
                self.0.name(n)
            }
            fn parent(&self, n: NodeId) -> Option<NodeId> {
                self.0.parent(n)
            }
            fn invocation_instant(&self) -> Option<(i64, u32)> {
                Some((self.1, self.2))
            }
        }
        let (s, n, _) = crate::temporal::parse_iso("2026-07-12T09:00:00Z").unwrap();
        let t = Clocked(MockTree::sample(), s, n);
        let got = match eval(&parse(&lex("^ | (now())").unwrap()).unwrap(), &t) {
            QueryResult::Values(vs) => vs,
            _ => panic!("expected values"),
        };
        assert_eq!(
            got,
            vec![Value::Instant {
                secs: s,
                nanos: n,
                offset_min: Some(0),
            }]
        );
        assert_eq!(got[0].to_string(), "2026-07-12T09:00:00Z");
    }

    #[test]
    fn temporal_arith_overflow_nulls() {
        let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::quantity::scale_expr;
        let inst = |secs| Value::Instant {
            secs,
            nanos: 0,
            offset_min: Some(0),
        };
        let dur = |secs| Value::Duration { secs, nanos: 0 };
        // Instant + Duration past i64::MAX seconds: null, never a
        // panic (debug) or wrapped garbage instant (release).
        assert!(matches!(
            arith(ArithOp::Add, &inst(i64::MAX), &dur(1), sc),
            Value::Null
        ));
        // Duration + Duration overflow.
        assert!(matches!(
            arith(ArithOp::Add, &dur(i64::MAX), &dur(1), sc),
            Value::Null
        ));
        // Instant − Instant underflow.
        assert!(matches!(
            arith(ArithOp::Sub, &inst(i64::MIN), &inst(i64::MAX), sc),
            Value::Null
        ));
        // The non-overflowing path is unchanged.
        assert_eq!(
            arith(ArithOp::Add, &inst(0), &dur(60), sc).to_string(),
            "1970-01-01T00:01:00Z"
        );
    }

    #[test]
    fn contains_and_regex_null_propagate() {
        let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::quantity::scale_expr;
        let name = Value::Str("chapter one".into());
        // A null right-hand side must NOT vacuously match (the bug:
        // Null stringified to "" is a substring of everything).
        assert!(!compare(&name, CmpOp::Contains, &Value::Null, sc));
        // An actual empty string still matches, as it should.
        assert!(compare(
            &name,
            CmpOp::Contains,
            &Value::Str(String::new()),
            sc
        ));
        // A real substring still matches.
        assert!(compare(
            &name,
            CmpOp::Contains,
            &Value::Str("one".into()),
            sc
        ));
        // `=~` against a null pattern doesn't match everything (the
        // bug: Regex::new("") matches any input).
        assert!(!compare(&name, CmpOp::Match, &Value::Null, sc));
        // `!~` against null is true, mirroring `!=` on null.
        assert!(compare(&name, CmpOp::NotMatch, &Value::Null, sc));
        // A real pattern still matches.
        assert!(compare(
            &name,
            CmpOp::Match,
            &Value::Str("^chap".into()),
            sc
        ));
    }
}
