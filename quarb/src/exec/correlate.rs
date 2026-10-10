//! Part of the executor: see `exec/mod.rs`.

use super::*;
use crate::adapter::{Conjunct, PlanAxis, StepPlan};

/// Evaluate `query` starting navigation from `start`, with `base`
/// correlation contexts already in scope. Prior `<=>` expressions are
/// evaluated first and appended to the trace.
pub(super) fn eval_query(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
) -> QueryResult {
    eval_query_outer(query, adapter, start, base, None)
}

/// [`eval_query`] with the invoking capsa's scope, when evaluating
/// a subcontext body — what `$.name` / `$$_` / `$$ord` reach.
pub(super) fn eval_query_outer(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
    outer: Option<&Scope<'_>>,
) -> QueryResult {
    // Whether the query is value-typed is a static property: it holds
    // if any branch projects (`::`) or any pipeline stage produces
    // values. This must not depend on how many nodes matched\,---\,
    // `//a::text` over a document with no `<a>` is still an (empty)
    // list of values, not a node set. Node-preserving stages
    // (positional selection, keyed aggregates) keep a node context a
    // node context.
    let (caps, projected) = eval_query_caps_outer(query, adapter, start, base, outer);
    to_result(caps, projected)
}

/// [`eval_query`]'s capsa-level form: the final capsae plus the
/// static value-vs-node typing, before reduction to a result.
/// The count an adapter answers for a query that is a chain of
/// bare child hops from `start` (or the root) ending in one step (a literal name or `*`, at
/// most one leading equality, no other predicates, no traits, no
/// marks) closed by `@| count` and nothing else — `/invoices/* @|
/// count`, `//token[::lower = "tom"] @| count`.
fn counted(query: &Query, adapter: &impl AstAdapter, start: NodeId) -> Option<usize> {
    if !query.correlations.is_empty() || query.branches.len() != 1 {
        return None;
    }
    let [Stage::Agg(call)] = query.pipeline.as_slice() else {
        return None;
    };
    if call.name != "count" || !call.args.is_empty() {
        return None;
    }
    let b = &query.branches[0];
    if b.projection.is_some() {
        return None;
    }
    let mut from = match b.anchor {
        Anchor::Current => start,
        Anchor::Root => adapter.root(),
        _ => return None,
    };
    let (last, lead) = b.steps.split_last()?;
    for elem in lead {
        let PathElem::Step(step) = elem else {
            return None;
        };
        let Matcher::Name(name) = &step.matcher else {
            return None;
        };
        if !matches!(step.axis, Axis::Child)
            || !step.predicates.is_empty()
            || !step.traits.is_empty()
            || step.leaf
        {
            return None;
        }
        let mut hops = adapter.children_named(adapter.child_host(from), name);
        if hops.len() != 1 {
            return None;
        }
        from = hops.pop()?;
    }
    let PathElem::Step(step) = last else {
        return None;
    };
    if !step.traits.is_empty() || step.leaf {
        return None;
    }
    let axis = match step.axis {
        Axis::Child => PlanAxis::Child,
        Axis::Descendant(Reach::All) => PlanAxis::Descendant,
        _ => return None,
    };
    let name = match &step.matcher {
        Matcher::Name(n) => Some(n.as_str()),
        Matcher::Any => None,
        _ => return None,
    };
    let conjuncts = match step.predicates.as_slice() {
        [] => Vec::new(),
        [Predicate::Expr(e)] => {
            let (prop, value) = indexable_equality(e)?;
            vec![Conjunct::Equals(prop, value)]
        }
        _ => return None,
    };
    let plan = StepPlan {
        traits: Vec::new(),
        cap: Default::default(),
        axis,
        name,
        conjuncts,
        limit: None,
        order: None,
    };
    adapter.count(adapter.child_host(from), &plan)
}

/// `@| count` over a plain path, counted as the walk goes: a query
/// that is one anchored branch of child and descendant steps (any
/// name, traits and per-node predicates; no positional predicate,
/// no mark, no projection) closed by `@| count` alone. The lead
/// steps keep a frontier of node ids; the last step's matches are
/// tested and counted one at a time, never collected, so a count
/// over a million nodes costs the walk and nothing per node. The
/// adapter's index answers the last step's `//name[::prop = lit]`
/// as it does on the materializing path.
fn streamed_count(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Option<usize> {
    // A nested query (a record's field, a predicate's operand)
    // evaluates under the invoking capsa's register, marks and
    // witness; only a top-level count streams.
    if outer.is_some() || !trace.contexts.is_empty() {
        return None;
    }
    if !query.correlations.is_empty() || query.branches.len() != 1 {
        return None;
    }
    let [Stage::Agg(call)] = query.pipeline.as_slice() else {
        return None;
    };
    if call.name != "count" || !call.args.is_empty() {
        return None;
    }
    let b = &query.branches[0];
    if b.projection.is_some() {
        return None;
    }
    let from = match b.anchor {
        Anchor::Current => start,
        Anchor::Root => adapter.root(),
        _ => return None,
    };
    let steps: Vec<&Step> = b
        .steps
        .iter()
        .map(|e| match e {
            PathElem::Step(s) => Some(s),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let (last, lead) = steps.split_last()?;
    let streamable = |step: &Step, last: bool| {
        (match step.axis {
            Axis::Child => true,
            // A descendant lead step would need the frontier
            // deduplicated across nested bases; the last step
            // handles that itself.
            Axis::Descendant(Reach::All) => last,
            _ => false,
        }) && step
            .predicates
            .iter()
            .all(|p| matches!(p, Predicate::Expr(_)))
    };
    if !lead.iter().all(|s| streamable(s, false)) || !streamable(last, true) {
        return None;
    }
    let edge_of = |n: NodeId| {
        Some(EdgeCtx {
            source: adapter.parent(n).unwrap_or(n),
            label: CHILD.into(),
            target: n,
        })
    };
    // The lead steps: a frontier of ids, each step's matches
    // tested and filtered as the walk would, deduplicated.
    let mut frontier = vec![from];
    for step in lead {
        let mut next: Vec<NodeId> = Vec::new();
        for &at in &frontier {
            let host = adapter.child_host(at);
            let found: Vec<NodeId> = match &step.matcher {
                Matcher::Name(name) if step.traits.is_empty() && !step.leaf => {
                    adapter.children_named(host, name)
                }
                _ => adapter
                    .children(host)
                    .into_iter()
                    .filter(|&c| matches_step(adapter, c, step))
                    .collect(),
            };
            let kept = apply_predicates(
                adapter,
                found,
                step,
                trace,
                outer,
                &[],
                &[],
                &[],
                |&n| n,
                |&n| edge_of(n),
            );
            next.extend(kept);
        }
        let mut seen = std::collections::HashSet::new();
        next.retain(|n| seen.insert(*n));
        frontier = next;
        if frontier.is_empty() {
            return Some(0);
        }
    }
    // The last step, counted one node at a time.
    let mut count = 0usize;
    let mut seen: Option<std::collections::HashSet<NodeId>> = (frontier.len() > 1
        && !matches!(last.axis, Axis::Child))
    .then(std::collections::HashSet::new);
    let mut tally = |n: NodeId, step: &Step, count: &mut usize| {
        if let Some(seen) = seen.as_mut()
            && !seen.insert(n)
        {
            return;
        }
        if step.predicates.is_empty() {
            *count += 1;
            return;
        }
        *count += apply_predicates(
            adapter,
            vec![n],
            step,
            trace,
            outer,
            &[],
            &[],
            &[],
            |&n| n,
            |&n| edge_of(n),
        )
        .len();
    };
    for &at in &frontier {
        match last.axis {
            Axis::Child => {
                let host = adapter.child_host(at);
                for c in adapter.children(host) {
                    if matches_step(adapter, c, last) {
                        tally(c, last, &mut count);
                    }
                }
            }
            _ => {
                // The adapter's index for `//name[::prop = lit]`,
                // as the materializing step uses it; the hit list
                // is ids alone.
                let mut consumed_first = false;
                let indexed = match &last.matcher {
                    Matcher::Name(n) => {
                        let conjuncts = match last.predicates.first() {
                            Some(Predicate::Expr(e)) => indexable_equality(e)
                                .map(|(prop, value)| vec![Conjunct::Equals(prop, value)])
                                .unwrap_or_default(),
                            _ => Vec::new(),
                        };
                        let plan = StepPlan {
                            traits: Vec::new(),
                            cap: Default::default(),
                            axis: PlanAxis::Descendant,
                            name: Some(n),
                            conjuncts,
                            limit: None,
                            order: None,
                        };
                        adapter.select(at, &plan).map(|sel| {
                            consumed_first = sel.applied.first().copied().unwrap_or(false);
                            sel.nodes
                        })
                    }
                    _ => None,
                };
                let rest;
                let step = if consumed_first {
                    rest = Step {
                        predicates: last.predicates[1..].to_vec(),
                        ..(*last).clone()
                    };
                    &rest
                } else {
                    *last
                };
                match indexed {
                    Some(hits) => {
                        for (n, _) in hits {
                            if tests_ok(adapter, n, step) {
                                tally(n, step, &mut count);
                            }
                        }
                    }
                    None => {
                        let mut stack: Vec<NodeId> = adapter.children(at);
                        stack.reverse();
                        while let Some(c) = stack.pop() {
                            if matches_step(adapter, c, step) {
                                tally(c, step, &mut count);
                            }
                            let kids = adapter.children(c);
                            stack.extend(kids.into_iter().rev());
                        }
                    }
                }
            }
        }
    }
    Some(count)
}

pub(crate) fn eval_query_caps(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
) -> (Vec<Capsa>, bool) {
    eval_query_caps_outer(query, adapter, start, base, None)
}

pub(super) fn eval_query_caps_outer(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
    outer: Option<&Scope<'_>>,
) -> (Vec<Capsa>, bool) {
    // `/name @| count` and `//name @| count` over one bare step: an
    // adapter that counts (a table's `COUNT(*)`, an index bucket)
    // answers without the nodes being made.
    if let Some(n) = counted(query, adapter, start) {
        return (
            vec![Capsa::bare(adapter.root(), Some(Value::Int(n as i64)))],
            true,
        );
    }
    // Any other `@| count` over a plain path: counted as the walk
    // goes, the matches never collected.
    if let Some(n) = streamed_count(query, adapter, start, base, outer) {
        return (
            vec![Capsa::bare(adapter.root(), Some(Value::Int(n as i64)))],
            true,
        );
    }
    let first_new = base.contexts.len();
    let mut contexts = base.contexts.clone();
    let mut outers = base.outer.clone();
    // Driver-first: the query's own branches drive; each joined
    // expression materializes as a context, its correlated final-
    // step predicates (the ON clause) lifted out for the gate below.
    let mut on_preds: Vec<Vec<PredExpr>> = Vec::new();
    for corr in &query.correlations {
        let (nav, on) = split_on(corr);
        let ctx = match eval_query(&nav, adapter, start, &Correlation::default()) {
            QueryResult::Nodes(ns) => ns,
            QueryResult::Values(_) => Vec::new(),
        };
        contexts.push(ctx);
        outers.push(corr.outer);
        on_preds.push(on);
    }
    let trace = Correlation {
        contexts,
        outer: outers,
        witnesses: Default::default(),
        join_keys: Default::default(),
    };
    let mut caps = union_branches(&query.branches, adapter, start, &trace, outer);
    // The join binds where it was written: the driver's stages
    // before the first `<=>` run first, the gate binds against the
    // capsae as they then stand, and the rest of the pipeline
    // follows.
    let at = query.join_at.min(query.pipeline.len());
    // The stage ordinal is the origin coordinate: set at the top
    // level only (a body's stages inherit the stage that runs it).
    let top = outer.is_none();
    caps = apply_stages(&query.pipeline[..at], 0, top, caps, adapter, &trace, outer);
    if !query.correlations.is_empty() {
        caps = correlate_gate(adapter, caps, &trace, first_new, &on_preds, outer);
    }
    caps = apply_stages(&query.pipeline[at..], at, top, caps, adapter, &trace, outer);
    (caps, pipeline_projected(query))
}

/// Split a joined expression into its plain navigation and its ON
/// clause: the final-step predicates that mention `$*k` or `$$…`
/// are lifted out, to be evaluated per driving capsa by
/// [`correlate_gate`]; everything else materializes once.
pub(super) fn split_on(entry: &Query) -> (Query, Vec<PredExpr>) {
    let mut nav = entry.clone();
    let mut on = Vec::new();
    for b in &mut nav.branches {
        if let Some(PathElem::Step(s)) = b.steps.last_mut() {
            s.predicates.retain_mut(|p| match p {
                Predicate::Expr(e)
                    if crate::parser::max_ctx_pred_expr(e) > 0
                        || crate::parser::pred_mentions_outer(e) =>
                {
                    on.push(e.clone());
                    false
                }
                _ => true,
            });
        }
    }
    (nav, on)
}

/// The join gate: for each driving capsa, search the joined
/// contexts for a tuple satisfying every ON clause — entry `i`'s
/// predicates evaluated against entry `i`'s candidate node, with
/// the driver reachable as `$$…` and earlier entries as `$*k`. The
/// first satisfying tuple (document order) is kept as the witness;
/// an outer entry (`<=>?`) offers a null binding after its real
/// candidates are exhausted, its own ON vacuous under it. A capsa
/// no tuple admits is dropped.
pub(super) fn correlate_gate(
    adapter: &impl AstAdapter,
    caps: Vec<Capsa>,
    trace: &Trace,
    first_new: usize,
    on_preds: &[Vec<PredExpr>],
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    caps.into_iter()
        .filter_map(|mut c| {
            let tuple = {
                let driver = Scope {
                    prov: Some(&c.prov),
                    register: &c.register,
                    members: &c.members,
                    topic: c.topic.as_ref(),
                    ordinal: None,
                    captures: &c.captures,
                    named: &c.named,
                    marks: &c.marks,
                    bindings: &[],
                    edge: None,
                    arrived: &c.arrived,
                    peers: None,
                    outer,
                    node: Some(c.node),
                };
                // Base contexts (an enclosing trace) stay unbound —
                // the placement rules keep ON clauses from
                // referencing them.
                let mut bound: Vec<Option<NodeId>> = vec![None; first_new];
                if bind_entries(adapter, &driver, on_preds, trace, first_new, &mut bound) {
                    Some(bound)
                } else {
                    None
                }
            };
            let tuple = tuple?;
            trace
                .witnesses
                .borrow_mut()
                .entry(c.node.0)
                .or_insert_with(|| tuple.clone());
            c.bindings = tuple;
            Some(c)
        })
        .collect()
}

/// The candidates of joined context `ctx_idx` worth testing for
/// one driver. Where the ON holds an equality between a property of
/// the candidate and a value of the driver (`::id = _::author_id`),
/// the context is indexed once by that property's equality key and
/// only the nodes under the driver's key are returned — with the
/// nodes whose value has no key, which must be tested every time —
/// in the context's own order, so the first witness is the one the
/// full scan would find. Every candidate is still tested against
/// the whole ON. Without such an equality, or when the driver's
/// value has no key, the whole context is returned: the nested
/// loop, as before.
pub(super) fn join_candidates(
    adapter: &impl AstAdapter,
    driver: &Scope<'_>,
    on: &[PredExpr],
    trace: &Trace,
    ctx_idx: usize,
    bound: &[Option<NodeId>],
) -> Vec<NodeId> {
    let context = &trace.contexts[ctx_idx];
    // A top-level conjunct `::p = <driver value>`, either way round.
    fn equality<'a>(e: &'a PredExpr) -> Option<(&'a str, &'a Operand, &'a Operand)> {
        match e {
            PredExpr::And(a, b) => equality(a).or_else(|| equality(b)),
            PredExpr::Compare(l, CmpOp::Eq, r) => {
                let own = |o: &'a Operand| match o {
                    Operand::Rel {
                        steps,
                        projection: Some(Projection::Property(Some(p))),
                        anchor: Anchor::Current,
                    } if steps.is_empty() => Some(p.as_str()),
                    _ => None,
                };
                match (own(l), r, own(r), l) {
                    (Some(p), Operand::Outer(_), _, _) => Some((p, l, r)),
                    (_, _, Some(p), Operand::Outer(_)) => Some((p, r, l)),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    let Some((prop, own_side, driver_side)) = on.iter().find_map(equality) else {
        return context.clone();
    };
    let scope = Scope {
        prov: None,
        register: driver.register,
        topic: driver.topic,
        captures: driver.captures,
        named: driver.named,
        marks: driver.marks,
        outer: Some(driver),
        ..NO_SCOPE
    };
    let at = driver.node.unwrap_or_else(|| adapter.root());
    let wanted = eval_operand(adapter, at, driver_side, trace, bound, scope);
    let keys: Option<Vec<String>> = wanted.iter().map(Value::equality_key).collect();
    let Some(keys) = keys else {
        return context.clone();
    };
    let memo_key = (ctx_idx, prop.to_string());
    let known = trace.join_keys.borrow().get(&memo_key).cloned();
    let index = match known {
        Some(ix) => ix,
        None => {
            let mut keyed: HashMap<String, Vec<usize>> = HashMap::new();
            let mut unkeyed = Vec::new();
            for (pos, &cand) in context.iter().enumerate() {
                let values = eval_operand(adapter, cand, own_side, trace, &[], NO_SCOPE);
                let ks: Option<Vec<String>> = values.iter().map(Value::equality_key).collect();
                match ks {
                    Some(ks) => {
                        for k in ks {
                            let at = keyed.entry(k).or_default();
                            if at.last() != Some(&pos) {
                                at.push(pos);
                            }
                        }
                    }
                    None => unkeyed.push(pos),
                }
            }
            let ix = std::rc::Rc::new(ValueKeys { keyed, unkeyed });
            trace.join_keys.borrow_mut().insert(memo_key, ix.clone());
            ix
        }
    };
    let mut at: Vec<usize> = index.unkeyed.clone();
    for k in keys {
        if let Some(hits) = index.keyed.get(&k) {
            at.extend(hits);
        }
    }
    at.sort_unstable();
    at.dedup();
    at.into_iter().map(|pos| context[pos]).collect()
}

/// Depth-first search over the joined contexts, one entry at a
/// time. `bound` holds the partial tuple; entry `i` binds context
/// `first_new + i`.
pub(super) fn bind_entries(
    adapter: &impl AstAdapter,
    driver: &Scope<'_>,
    on_preds: &[Vec<PredExpr>],
    trace: &Trace,
    first_new: usize,
    bound: &mut Vec<Option<NodeId>>,
) -> bool {
    let i = bound.len() - first_new;
    if i == on_preds.len() {
        return true;
    }
    let ctx_idx = bound.len();
    let candidates: Vec<NodeId> =
        join_candidates(adapter, driver, &on_preds[i], trace, ctx_idx, bound);
    for cand in candidates {
        bound.push(Some(cand));
        // One capsa, two nodes: the condition reads the driver's
        // register, marks and captures with the candidate as the
        // current node; `_` is the driver.
        let scope = Scope {
            prov: None,
            register: driver.register,
            topic: driver.topic,
            captures: driver.captures,
            named: driver.named,
            marks: driver.marks,
            outer: Some(driver),
            ..NO_SCOPE
        };
        let ok = on_preds[i]
            .iter()
            .all(|e| eval_pred_expr(adapter, cand, e, trace, bound, scope))
            && bind_entries(adapter, driver, on_preds, trace, first_new, bound);
        if ok {
            return true;
        }
        bound.pop();
    }
    if trace.outer.get(ctx_idx).copied().unwrap_or(false) {
        bound.push(None);
        if bind_entries(adapter, driver, on_preds, trace, first_new, bound) {
            return true;
        }
        bound.pop();
    }
    false
}

/// Whether a pipeline stage turns the context value-typed. Positional
/// selection, per-capsa filters, and keyed aggregates reorder or
/// filter capsae without producing values; every other stage does.
pub(super) fn stage_projects(stage: &Stage) -> bool {
    match stage {
        Stage::Select(_) | Stage::Filter(_) => false,
        // A bare push leaves the topic as it was — the runtime
        // any-topic check in [`to_result`] carries the typing —
        // and in node context it MARKS, which must not value-type
        // the query. (The computed pushes set the topic and stay
        // value-typing, in the default arm.)
        Stage::Push(_) => false,
        // A navigation stage re-types by its own projection —
        // handled in the [`pipeline_projected`] fold (it can also
        // RESET a projected pipeline back to nodes).
        Stage::Nav(b) => b.projection.is_some(),
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
pub(super) fn pipeline_projected(query: &Query) -> bool {
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
            // A navigation stage rebuilds the capsae: without a
            // projection the context is node-typed again no matter
            // what came before; with one it is value-typed.
            Stage::Nav(b) => projected = b.projection.is_some(),
            s => projected = projected || stage_projects(s),
        }
    }
    projected
}

/// Navigate every branch from `start`, projecting where asked, and
/// union the resulting capsae (deduped by node).
pub(super) fn union_branches(
    branches: &[Branch],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    let mut caps = Vec::new();
    for branch in branches {
        // A root-anchored branch (`^` / `()`) navigates from the
        // root — the same node at the top level, the reach-back
        // from inside a subcontext body. The mark anchors navigate
        // from the invoking thread's marked node(s) — the plural
        // forms fork one navigation per pocket; the invoker's
        // marks also seed the navigation, so nested predicates and
        // deeper anchors keep seeing them.
        // A body forks the capsa it serves, as a hop does: the same
        // register, marks and captures ride in; the node is new.
        let seed: &[Mark] = outer.map(|s| s.marks).unwrap_or(&[]);
        let seed_register: &[Reg] = outer.map(|s| s.register).unwrap_or(&[]);
        // The scope an id anchor's expression evaluates in: the
        // invoker's, at the top level none.
        let anchor_scope = Scope {
            marks: seed,
            register: seed_register,
            ..outer.copied().unwrap_or(NO_SCOPE)
        };
        for from in anchor_nodes(adapter, &branch.anchor, start, trace, &[], anchor_scope) {
            for (node, register, marks, arrived) in navigate_paths(
                &branch.steps,
                adapter,
                from,
                trace,
                outer,
                &[],
                seed,
                seed_register,
            ) {
                caps.push(Capsa {
                    prov: if branch.projection.is_some() {
                        Prov::at(node, stage_now())
                    } else {
                        Prov::default()
                    },
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
                    captures: outer.map(|s| s.captures.to_vec()).unwrap_or_default(),
                    named: outer.map(|s| s.named.to_vec()).unwrap_or_default(),
                    bindings: trace
                        .witnesses
                        .borrow()
                        .get(&node.0)
                        .cloned()
                        .unwrap_or_default(),
                });
            }
        }
    }
    // Dedup by (node, register): per-path results — the same node
    // reached along walks with different breadcrumbs — stay
    // distinct; without pushes every key is (node, empty) and the
    // old node dedup is preserved.
    dedup_merge(caps)
}

/// Dedup capsae by (node, register), first-seen order, merging a
/// dropped duplicate's origins into the survivor: the same value
/// reached along another walk was read there too.
pub(super) fn dedup_merge(caps: Vec<Capsa>) -> Vec<Capsa> {
    let mut kept: Vec<Capsa> = Vec::new();
    let mut index: HashMap<(NodeId, RegKey), usize> = HashMap::new();
    for c in caps {
        let key = (c.node, reg_key(&c.register));
        match index.get(&key) {
            Some(&i) => merge_prov(&mut kept[i], &c),
            None => {
                index.insert(key, kept.len());
                kept.push(c);
            }
        }
    }
    kept
}

/// Fold `other`'s origins (topic and regulae) into `into`.
pub(super) fn merge_prov(into: &mut Capsa, other: &Capsa) {
    into.prov.origins.union(&other.prov.origins);
    for (a, b) in into.register.iter_mut().zip(&other.register) {
        a.prov.origins.union(&b.prov.origins);
    }
}
