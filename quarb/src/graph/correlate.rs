//! Part of the executor: see `exec/mod.rs`.

use super::*;
use crate::graph::adapter::{Conjunct, PlanAxis, StepPlan};

/// Evaluate `query` starting navigation from `start`, with `base`
/// correlation contexts already in scope. Prior `<=>` expressions are
/// evaluated first and appended to the trace.
pub(crate) fn eval_query(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
) -> QueryResult {
    eval_query_outer(query, adapter, start, base, None)
}

/// [`eval_query`] with the invoking capsa's scope, when evaluating
/// a subcontext body — what `$.name` / `$$_` / `$$ord` reach.
pub(crate) fn eval_query_outer(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
) -> QueryResult {
    // Whether the query is value-typed is a static property: it holds
    // if any branch projects (`::`) or any pipeline command produces
    // values. This must not depend on how many nodes matched\,---\,
    // `//a::text` over a document with no `<a>` is still an (empty)
    // list of values, not a node set. Node-preserving commands
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
    let [Command::Agg(call)] = query.pipeline.as_slice() else {
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
/// no mark; a projection allowed, since `count` counts its inputs
/// and a null value is one) closed by `@| count` alone. The lead
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
    outer: Option<&Scope<'_, GraphExt>>,
) -> Option<(usize, Origins)> {
    // A nested query (a record's field, a predicate's operand)
    // evaluates under the invoking capsa's register, marks and
    // witness; only a top-level count streams.
    if outer.is_some() || !trace.contexts.is_empty() {
        return None;
    }
    if !query.correlations.is_empty() || query.branches.len() != 1 {
        return None;
    }
    let [Command::Agg(call)] = query.pipeline.as_slice() else {
        return None;
    };
    if call.name != "count" || !call.args.is_empty() {
        return None;
    }
    let b = &query.branches[0];
    // A projected path (`//token<word>::lower @| count`) counts its
    // nodes too — `count` is the number of inputs, a null value
    // among them — so the value is never made; what a projection
    // changes is the provenance: a computed reduction over values
    // keeps the union of their origins, so the counted nodes are
    // recorded (bounded, as every origins set is).
    let projected = b.projection.is_some();
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
    let streamable = |step: &Step, _last: bool| {
        // A descendant lead step walks its subtree for the frontier
        // (the adapter's name index where it has one); nested bases
        // are deduplicated there, and the last step's tally
        // deduplicates its own matches across them.
        matches!(step.axis, Axis::Child | Axis::Descendant(Reach::All))
            && step
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
            row: None,
        })
    };
    // The lead steps: a frontier of ids, each step's matches
    // tested and filtered as the walk would, deduplicated.
    let mut frontier = vec![from];
    for step in lead {
        let mut next: Vec<NodeId> = Vec::new();
        for &at in &frontier {
            let host = adapter.child_host(at);
            let found: Vec<NodeId> = match step.axis {
                Axis::Child => match &step.matcher {
                    Matcher::Name(name) if step.traits.is_empty() && !step.leaf => {
                        adapter.children_named(host, name)
                    }
                    _ => adapter
                        .children(host)
                        .into_iter()
                        .filter(|&c| matches_step(adapter, c, step))
                        .collect(),
                },
                // `//paragraph//token @| count`: the lead's
                // descendants by the adapter's name index where it
                // keeps one, else a walk of the subtree in document
                // order, matching as the step would — ids only, so
                // a million-token subtree costs the walk.
                _ => {
                    let indexed = match &step.matcher {
                        Matcher::Name(name) if step.traits.is_empty() && !step.leaf => adapter
                            .descendants_named(host, name)
                            .map(|v| v.into_iter().map(|(n, _)| n).collect::<Vec<_>>()),
                        _ => None,
                    };
                    indexed.unwrap_or_else(|| {
                        let mut out = Vec::new();
                        let mut stack: Vec<NodeId> = adapter.children(host);
                        stack.reverse();
                        while let Some(n) = stack.pop() {
                            if matches_step(adapter, n, step) {
                                out.push(n);
                            }
                            let mut kids = adapter.children(adapter.child_host(n));
                            kids.reverse();
                            stack.extend(kids);
                        }
                        out
                    })
                }
            };
            let kept = apply_predicates(
                adapter,
                found,
                step,
                trace,
                outer,
                &[],
                &GraphExt::at(adapter.root()),
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
            return Some((0, Origins::default()));
        }
    }
    // The last step, counted one node at a time.
    let mut count = 0usize;
    let mut origins = Origins::default();
    let command = command_now();
    let mut seen: Option<std::collections::HashSet<NodeId>> = (frontier.len() > 1
        && !matches!(last.axis, Axis::Child))
    .then(std::collections::HashSet::new);
    let mut tally = |n: NodeId, step: &Step, count: &mut usize, origins: &mut Origins| {
        if let Some(seen) = seen.as_mut()
            && !seen.insert(n)
        {
            return;
        }
        let kept = if step.predicates.is_empty() {
            1
        } else {
            apply_predicates(
                adapter,
                vec![n],
                step,
                trace,
                outer,
                &[],
                &GraphExt::at(adapter.root()),
                &[],
                |&n| n,
                |&n| edge_of(n),
            )
            .len()
        };
        *count += kept;
        if projected && kept > 0 {
            origins.insert(Origin {
                position: (n).into(),
                command: command,
            });
        }
    };
    for &at in &frontier {
        match last.axis {
            Axis::Child => {
                let host = adapter.child_host(at);
                for c in adapter.children(host) {
                    if matches_step(adapter, c, last) {
                        tally(c, last, &mut count, &mut origins);
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
                                tally(n, step, &mut count, &mut origins);
                            }
                        }
                    }
                    None => {
                        let mut stack: Vec<NodeId> = adapter.children(at);
                        stack.reverse();
                        while let Some(c) = stack.pop() {
                            if matches_step(adapter, c, step) {
                                tally(c, step, &mut count, &mut origins);
                            }
                            let kids = adapter.children(c);
                            stack.extend(kids.into_iter().rev());
                        }
                    }
                }
            }
        }
    }
    Some((count, origins))
}

pub(crate) fn eval_query_caps(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
) -> (Vec<Capsa>, bool) {
    eval_query_caps_outer(query, adapter, start, base, None)
}

pub(crate) fn eval_query_caps_outer(
    query: &Query,
    adapter: &impl AstAdapter,
    start: NodeId,
    base: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
) -> (Vec<Capsa>, bool) {
    // `/name @| count` and `//name @| count` over one bare step: an
    // adapter that counts (a table's `COUNT(*)`, an index bucket)
    // answers without the nodes being made.
    if let Some(n) = counted(query, adapter, start) {
        return (
            vec![Capsa::bare(
                GraphExt::at(adapter.root()),
                Some(Value::Int(n as i64)),
            )],
            true,
        );
    }
    // Any other `@| count` over a plain path: counted as the walk
    // goes, the matches never collected.
    if let Some((n, origins)) = streamed_count(query, adapter, start, base, outer) {
        let mut c = Capsa::bare(GraphExt::at(adapter.root()), Some(Value::Int(n as i64)));
        if !origins.is_empty() {
            c.prov = Prov::leaf(origins);
        }
        return (vec![c], true);
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
    // A collapsing reducer or a graph table right after the branches
    // reads nodes alone: the walk may expand by node where that is
    // exact (spec: Expansion paths, Acyclic links).
    let node_level = matches!(
        query.pipeline.first(),
        Some(Command::Agg(call)) if matches!(call.name.as_str(), "collapse" | "nodes" | "edges")
    );
    let mut caps = union_branches(&query.branches, adapter, start, &trace, outer, node_level);
    // The expression head's thread opens with the topic the run was
    // given (`qc` reading its pipe): the top-level query only, and
    // only the head's shape — one row from the root anchor with no
    // navigation — so a `^` anchor inside a body keeps a null topic.
    if outer.is_none()
        && query.correlations.is_empty()
        && !query.branches.is_empty()
        && query.branches.iter().all(|b| {
            matches!(b.anchor, Anchor::Root) && b.steps.is_empty() && b.projection.is_none()
        })
    {
        if let Some(seed) = run_option(|o| o.topic.clone()) {
            for c in caps.iter_mut().filter(|c| c.topic.is_none()) {
                c.topic = Some(seed.clone());
            }
        }
    }
    // The join binds where it was written: the driver's commands
    // before the first `<=>` run first, the gate binds against the
    // capsae as they then stand, and the rest of the pipeline
    // follows.
    let at = query.join_at.min(query.pipeline.len());
    // The command ordinal is the origin coordinate: set at the top
    // level only (a body's commands inherit the command that runs it).
    let top = outer.is_none();
    caps = apply_commands(&query.pipeline[..at], 0, top, caps, adapter, &trace, outer);
    if !query.correlations.is_empty() {
        caps = correlate_gate(adapter, caps, &trace, first_new, &on_preds, outer);
    }
    caps = apply_commands(&query.pipeline[at..], at, top, caps, adapter, &trace, outer);
    (caps, pipeline_projected(query))
}

/// Split a joined expression into its plain navigation and its ON
/// clause: the final-step predicates that mention `$*k` or `$$…`
/// are lifted out, to be evaluated per driving capsa by
/// [`correlate_gate`]; everything else materializes once.
pub(crate) fn split_on(entry: &Query) -> (Query, Vec<PredExpr>) {
    let mut nav = entry.clone();
    let mut on = Vec::new();
    for b in &mut nav.branches {
        if let Some(PathElem::Step(s)) = b.steps.last_mut() {
            s.predicates.retain_mut(|p| match p {
                Predicate::Expr(e)
                    if crate::syntax::parser::max_ctx_pred_expr(e) > 0
                        || crate::syntax::parser::pred_mentions_outer(e) =>
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
pub(crate) fn correlate_gate(
    adapter: &impl AstAdapter,
    caps: Vec<Capsa>,
    trace: &Trace,
    first_new: usize,
    on_preds: &[Vec<PredExpr>],
    outer: Option<&Scope<'_, GraphExt>>,
) -> Vec<Capsa> {
    caps.into_iter()
        .filter_map(|mut c| {
            let tuple = {
                // The driver's state as the ON clause sees it: its
                // marks and arrivals, borrowed, under no witness (that
                // is what is being found): the empty witness in the
                // slot hides the driver's own bindings.
                let driver = Scope {
                    prov: Some(&c.prov),
                    register: &c.register,
                    members: &c.members,
                    topic: c.topic.as_ref(),
                    ordinal: None,
                    captures: &c.captures,
                    named: &c.named,
                    peers: None,
                    outer,
                    driver: None,
                    ext: &c.ext,
                    view: c.ext.view(),
                    crossing: None,
                    walk: None,
                    witness: Some(&[]),
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
                .entry(c.ext.node.0)
                .or_insert_with(|| tuple.clone());
            c.ext.bindings = tuple;
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
pub(crate) fn join_candidates(
    adapter: &impl AstAdapter,
    driver: &Scope<'_, GraphExt>,
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
                    (Some(p), Operand::Driver(_), _, _) => Some((p, l, r)),
                    (_, _, Some(p), Operand::Driver(_)) => Some((p, r, l)),
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
        driver: Some(driver),
        outer: driver.outer,
        ..Scope::at(driver.ext)
    };
    let at = driver.view.node.unwrap_or_else(|| adapter.root());
    let wanted = eval_operand_at(adapter, at, driver_side, trace, bound, scope);
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
                let values = eval_operand_at(
                    adapter,
                    cand,
                    own_side,
                    trace,
                    &[],
                    Scope::at(&GraphExt::at(cand)),
                );
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
pub(crate) fn bind_entries(
    adapter: &impl AstAdapter,
    driver: &Scope<'_, GraphExt>,
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
            driver: Some(driver),
            outer: driver.outer,
            ..Scope::at(driver.ext)
        };
        let ok = on_preds[i]
            .iter()
            .all(|e| eval_pred_expr_at(adapter, cand, e, trace, bound, scope))
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

/// Navigate every branch from `start`, projecting where asked, and
/// union the resulting capsae (deduped by node).
pub(crate) fn union_branches(
    branches: &[Branch],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    node_level: bool,
) -> Vec<Capsa> {
    let mut caps = Vec::new();
    for branch in branches {
        // A root-anchored branch (`^` / `()`) navigates from the
        // root — the same node at the top level, the reach-back
        // from inside a subcontext body. The mark anchors navigate
        // from the invoking thread's marked node(s) — the plural
        // forms fork one navigation per pocket; the invoker's
        // register (references included) also seeds the navigation,
        // so nested predicates and deeper anchors keep seeing them.
        // A body forks the capsa it serves, as a hop does: the same
        // register and captures ride in; the node is new.
        let top = GraphExt::at(start);
        let seed_register: &[Reg] = outer.map(|s| s.register).unwrap_or(&[]);
        // The scope an id anchor's expression evaluates in: the
        // invoker's, at the top level none.
        let anchor_scope = Scope {
            register: seed_register,
            ..outer.copied().unwrap_or(Scope::at(&top))
        };
        for from in anchor_nodes(adapter, &branch.anchor, start, trace, &[], anchor_scope) {
            for (ext, register) in navigate_paths(
                &branch.steps,
                adapter,
                from,
                trace,
                outer,
                &[],
                seed_register,
                node_level && branch.projection.is_none(),
            ) {
                caps.push(Capsa {
                    prov: if branch.projection.is_some() {
                        Prov::at(ext.node.into(), command_now())
                    } else {
                        Prov::default()
                    },
                    register,
                    topic: branch
                        .projection
                        .as_ref()
                        .map(|p| project(adapter, ext.node, p)),
                    members: Vec::new(),
                    captures: outer.map(|s| s.captures.to_vec()).unwrap_or_default(),
                    named: outer.map(|s| s.named.to_vec()).unwrap_or_default(),
                    ext: GraphExt {
                        bindings: trace
                            .witnesses
                            .borrow()
                            .get(&ext.node.0)
                            .cloned()
                            .unwrap_or_default(),
                        ..ext
                    },
                });
            }
        }
    }
    // Dedup the indistinguishable alone (spec: Expansion paths):
    // the same node reached by the same edge with the same facts
    // and the same register is one thread; another route, another
    // edge or another capsa is another thread.
    dedup_merge(caps)
}

/// The identity a thread's arrival has for the dedup: its arriving
/// link edges and the walk's facts, where a walk ended here. A tree
/// edge (child, parent, sibling) is no part of it: the tree axes
/// answer a node set, one thread per node however many contexts
/// climbed, descended or stepped sideways to it.
pub(crate) type ArrivalKey = (
    Vec<(NodeId, String, NodeId, Option<NodeId>)>,
    Option<(usize, usize, Option<u64>, u64)>,
);

pub(crate) fn arrival_key(ext: &GraphExt) -> ArrivalKey {
    (
        ext.arrived
            .iter()
            .filter(|e| !matches!(e.label.as_str(), CHILD | PARENT | NEXT | PREV))
            // the edge's row keeps two rows on one leg two threads
            .map(|e| (e.source, e.label.clone(), e.target, e.row))
            .collect(),
        ext.walk.as_ref().map(|w| {
            (
                w.round,
                w.hops,
                w.total.as_ref().map(|(v, ..)| v.to_bits()),
                w.trail,
            )
        }),
    )
}

/// Dedup capsae by (node, arrival, register), first-seen order,
/// merging a dropped duplicate's origins into the survivor: the
/// same value reached the same way was read there too.
pub(crate) fn dedup_merge(caps: Vec<Capsa>) -> Vec<Capsa> {
    let mut kept: Vec<Capsa> = Vec::new();
    let mut index: HashMap<(NodeId, ArrivalKey, RegKey), usize> = HashMap::new();
    for c in caps {
        let key = (c.ext.node, arrival_key(&c.ext), reg_key(&c.register));
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
pub(crate) fn merge_prov(into: &mut Capsa, other: &Capsa) {
    into.prov.origins.union(&other.prov.origins);
    for (a, b) in into.register.iter_mut().zip(&other.register) {
        a.prov.origins.union(&b.prov.origins);
    }
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
    pub(crate) outer: Vec<bool>,
    pub(crate) contexts: Vec<Vec<NodeId>>,
    pub(crate) witnesses: std::cell::RefCell<std::collections::HashMap<u64, Vec<Option<NodeId>>>>,
    /// Per joined context and property: the context's nodes by the
    /// equality key of that property — the hash side of a join
    /// whose ON holds `::p = _::q`, built on first use.
    pub(crate) join_keys:
        std::cell::RefCell<std::collections::HashMap<(usize, String), std::rc::Rc<ValueKeys>>>,
}

pub(crate) type Trace = Correlation;
