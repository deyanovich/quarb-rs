//! Part of the executor: see `exec/mod.rs`.

use super::*;
use crate::graph::adapter::{Conjunct, PlanAxis, StepPlan};

/// Run the navigation path starting from `start`, per-path: each
/// result carries the register its expansion path's pushes built.
/// Paths without pattern pushes carry empty registers and dedup by
/// node — the plain semantics, unchanged. `witness` is the invoking
/// thread's correlation tuple (empty where there is none): step
/// predicates and pattern guards along the way read `$*k` under it.
pub(crate) fn navigate_paths(
    elems: &[PathElem],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    seed_register: &[Reg],
) -> Vec<(GraphExt, Vec<Reg>)> {
    // The frontier: one attachment per path (its node, its marks, the
    // edges it arrived by) with the path's register beside it. The
    // witness rides in the scope's slot, shared by every path.
    let mut ctx: Vec<(GraphExt, Vec<Reg>)> = vec![(
        GraphExt {
            node: start,
            bindings: Vec::new(),
            arrived: Vec::new(),
            unplaced: false,
        },
        seed_register.to_vec(),
    )];
    for elem in elems {
        // A mark files a reference to each in-flight path's node.
        if let PathElem::Mark(name) = elem {
            for (ext, register) in &mut ctx {
                register.push(mark_reg(adapter, ext.node, name.clone()));
            }
            continue;
        }
        let mut next: Vec<(GraphExt, Vec<Reg>)> = Vec::new();
        if let PathElem::Step(step) = elem {
            prefetch_hint(adapter, step, ctx.iter().map(|c| c.0.node));
        }
        for (ext, register) in &ctx {
            let node = ext.node;
            match elem {
                PathElem::Step(step) => {
                    let mut succs = Vec::new();
                    apply_step(
                        adapter, node, step, trace, outer, witness, ext, register, &mut succs,
                    );
                    next.extend(succs.into_iter().map(|s| {
                        let arrived = arrived_edge(adapter, node, step, s).into_iter().collect();
                        (
                            GraphExt {
                                node: s,
                                bindings: Vec::new(),
                                arrived,
                                unplaced: false,
                            },
                            register.clone(),
                        )
                    }));
                }
                PathElem::Group(group) => {
                    let anchor = GPath {
                        ext: GraphExt {
                            node,
                            bindings: Vec::new(),
                            arrived: Vec::new(),
                            unplaced: false,
                        },
                        visited: vec![node],
                        register: register.clone(),
                        last_edge: None,
                    };
                    next.extend(
                        expand_group(adapter, group, anchor, trace, outer, witness)
                            .into_iter()
                            .map(|p| {
                                let arrived = p.last_edge.clone().into_iter().collect();
                                (GraphExt { arrived, ..p.ext }, p.register)
                            }),
                    );
                }
                PathElem::Mark(_) => unreachable!("handled above"),
                PathElem::Push { .. } => {}
            }
        }
        let mut merged: Vec<(GraphExt, Vec<Reg>)> = Vec::new();
        let mut index: HashMap<
            (NodeId, Vec<(Option<String>, String)>),
            usize,
        > = HashMap::new();
        for (ext, r) in next {
            match index.entry((ext.node, reg_key(&r))) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    let kept = &mut merged[*e.get()];
                    for (a, b) in kept.1.iter_mut().zip(&r) {
                        a.prov.origins.union(&b.prov.origins);
                    }
                    let slot = &mut kept.0.arrived;
                    for edge in ext.arrived {
                        if !slot.contains(&edge) {
                            slot.push(edge);
                        }
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(merged.len());
                    merged.push((ext, r));
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

pub(crate) fn nested_outer<'a>(scope: &'a Scope<'a, GraphExt>) -> Option<&'a Scope<'a, GraphExt>> {
    if scope.view.node.is_some() {
        Some(scope)
    } else {
        scope.outer
    }
}

/// A scope path with its predicates dropped: every node the path
/// could select, whatever the data holds — what a question about
/// declarations is asked of.
pub(crate) fn without_predicates(elems: &[PathElem]) -> Vec<PathElem> {
    elems
        .iter()
        .map(|e| match e {
            PathElem::Step(s) => {
                let mut s = s.clone();
                s.predicates.clear();
                PathElem::Step(s)
            }
            other => other.clone(),
        })
        .collect()
}

pub(crate) fn navigate_from(
    elems: &[PathElem],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    seed_register: &[Reg],
) -> Vec<NodeId> {
    dedup(
        navigate_paths(
            elems,
            adapter,
            start,
            trace,
            outer,
            witness,
            seed_register,
        )
        .into_iter()
        .map(|(e, _)| e.node)
        .collect(),
    )
}

/// One in-flight expansion path through a path-pattern group: the
/// current node, every node the path has traversed (the group's
/// anchor included), the breadcrumbs its pushes have made, and the
/// edge it most recently walked (the `$-` a pattern command sees).
/// Simple-path semantics: a hop whose target is already in `visited`
/// is not taken — which is what makes open quantifiers terminate on
/// cyclic crosslink data. `visited` is kept sorted (traversal order
/// carries no meaning for the cycle check), so two paths that cover
/// the same ground — and pushed the same breadcrumbs — compare equal
/// and dedup.
#[derive(Clone)]
pub(crate) struct GPath {
    /// The path's node and marks, as the attachment a scope borrows.
    pub(crate) ext: GraphExt,
    pub(crate) visited: Vec<NodeId>,
    pub(crate) register: Vec<Reg>,
    pub(crate) last_edge: Option<EdgeCtx>,
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
            ext: GraphExt {
                node,
                bindings: Vec::new(),
                arrived: Vec::new(),
                unplaced: false,
            },
            visited,
            register: self.register.clone(),
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
            self.ext.node,
            self.visited.clone(),
            reg_key(&self.register),
            self.last_edge
                .as_ref()
                .map(|e| (e.source, e.label.clone(), e.target)),
        )
    }
}

/// The edge `step` walked from `from` to `succ` — the `$-` referent
/// a pattern command sees. Crosslink labels are re-derived from the
/// adapter (first matching edge; the parallel-edge caveat of
/// `link_property` applies).
pub(crate) fn arrived_edge(
    adapter: &impl AstAdapter,
    from: NodeId,
    step: &Step,
    succ: NodeId,
) -> Option<EdgeCtx> {
    let edge = match &step.axis {
        Axis::Child | Axis::Descendant(_) | Axis::Universal => EdgeCtx {
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
            let label = select_links(step, &links_of(adapter, from))
                .into_iter()
                .find(|(_, t)| *t == succ)
                .map(|(l, _)| l)?;
            EdgeCtx {
                source: from,
                label,
                target: succ,
            }
        }
        Axis::InLink => {
            let label = select_links(step, &backlinks_of(adapter, from))
                .into_iter()
                .find(|(_, s)| *s == succ)
                .map(|(l, _)| l)?;
            EdgeCtx {
                source: succ,
                label,
                target: from,
            }
        }
        // The headless arrow: the edge context still records the
        // edge's true direction — the walk just declined to assert
        // it. Outgoing is tried first.
        Axis::BothLink => {
            if let Some(label) = select_links(step, &links_of(adapter, from))
                .into_iter()
                .find(|(_, t)| *t == succ)
                .map(|(l, _)| l)
            {
                EdgeCtx {
                    source: from,
                    label,
                    target: succ,
                }
            } else {
                let label = select_links(step, &backlinks_of(adapter, from))
                    .into_iter()
                    .find(|(_, s)| *s == succ)
                    .map(|(l, _)| l)?;
                EdgeCtx {
                    source: succ,
                    label,
                    target: from,
                }
            }
        }
        Axis::Resolve { property, .. } => EdgeCtx {
            source: from,
            label: property.clone().unwrap_or_else(|| "ref".to_string()),
            target: succ,
        },
        Axis::ReverseResolve { property, .. } => EdgeCtx {
            source: succ,
            label: match property {
                RefProp::Named(p) => p.clone(),
                _ => adapter
                    .ref_property(succ)
                    .unwrap_or_else(|| "ref".to_string()),
            },
            target: from,
        },
        Axis::ValueMatch {
            property, target, ..
        } => EdgeCtx {
            source: from,
            label: format!("{property}={target}"),
            target: succ,
        },
    };
    Some(edge)
}

/// Expand a path-pattern group from one anchor path. Repetitions run
/// from 1 to the quantifier's ceiling (open forms: the adapter's
/// bound `N_max`); each repetition is the union over the group's
/// alternatives; results collect at every count `>= min`, with
/// count 0 contributing the anchor itself when the quantifier
/// admits zero repetitions (`*`, `{0,n}`). The group's reach then
/// keeps all matches, or only those at the smallest (`?`) or
/// largest (`!`) repetition count.
///
/// The bound is a refusal, not an ellipsis: an explicit `{m,n}`
/// beyond `N_max` refuses up front, and an open-ended walk that
/// still has edges to take when `N_max` rounds are spent refuses
/// rather than passing off its first `N_max` rounds as the
/// closure. A walk whose frontier drains before the bound — or a
/// proximal reach that found its first tier — is complete and
/// stays silent.
pub(crate) fn expand_group(
    adapter: &impl AstAdapter,
    group: &Group,
    anchor: GPath,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
) -> Vec<GPath> {
    let n_max = quantifier_bound(adapter);
    if let Some(n) = group.quant.max {
        if n > n_max {
            record_refusal(format!(
                "the pattern's explicit ceiling ({n}) exceeds the \
                 quantifier bound ({n_max}); raise --quantifier-bound"
            ));
            return Vec::new();
        }
    } else if group.quant.min > n_max {
        record_refusal(format!(
            "the pattern's minimum ({}) exceeds the quantifier bound \
             ({n_max}); raise --quantifier-bound",
            group.quant.min
        ));
        return Vec::new();
    }
    let hi = group.quant.max.unwrap_or(n_max);
    // The group's predicates filter matches BEFORE reach — the
    // walk continues from the full frontier, but only survivors
    // are candidates. `(...)+[P]?` is therefore "the proximal
    // satisfying P": a shortest-path search.
    let admits = |p: &GPath| group_admits(adapter, group, p, trace, outer, witness);
    let mut matches: Vec<(GPath, usize)> = Vec::new();
    if group.quant.min == 0 && admits(&anchor) {
        matches.push((anchor.clone(), 0));
    }
    let mut frontier = vec![anchor];
    let mut answered = false;
    for count in 1..=hi {
        let mut next = Vec::new();
        for path in &frontier {
            for alt in &group.alts {
                next.extend(expand_elems(
                    adapter,
                    alt,
                    vec![path.clone()],
                    trace,
                    outer,
                    witness,
                ));
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
                answered = true;
                break;
            }
        }
    }
    // An open-ended walk that spent all N_max rounds with paths
    // still in flight: probe one more round. If anything is still
    // walkable the closure is truncated — refuse out loud instead
    // of returning it as if complete.
    if group.quant.max.is_none() && !answered && !frontier.is_empty() {
        let mut probe = Vec::new();
        for path in &frontier {
            for alt in &group.alts {
                probe.extend(expand_elems(
                    adapter,
                    alt,
                    vec![path.clone()],
                    trace,
                    outer,
                    witness,
                ));
                if !probe.is_empty() {
                    break;
                }
            }
            if !probe.is_empty() {
                break;
            }
        }
        if !probe.is_empty() {
            record_refusal(format!(
                "quantifier bound reached at round {n_max} with edges \
                 still to take — the closure would be silently \
                 incomplete; raise --quantifier-bound"
            ));
            return Vec::new();
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

/// `::name` on a node: the document's property, or — when the
/// adapter declares the key an alias and the node
/// has no such property — its metadata. Data always wins.
pub(crate) fn property_or_alias(adapter: &impl AstAdapter, node: NodeId, name: &str) -> Value {
    if let Some(v) = adapter.property(node, name) {
        return v;
    }
    if adapter.aliased_metadata(node).contains(&name)
        && let Some(v) = adapter.metadata(node, name)
    {
        return v;
    }
    Value::Null
}

/// Whether a path's endpoint passes the group's predicates, with
/// the path's arrived-by edge in scope as `$-`.
pub(crate) fn group_admits(
    adapter: &impl AstAdapter,
    group: &Group,
    path: &GPath,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
) -> bool {
    if !group.traits.is_empty() && !trait_clauses_ok(adapter, path.ext.node, &group.traits) {
        return false;
    }
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
    // The path's own register: the marks the walk filed are read
    // from it (`$$.x` inside the group's guard).
    let scope = Scope {
        prov: None,
        outer,
        register: &path.register,
        crossing: path.last_edge.as_ref(),
        witness: (!witness.is_empty()).then_some(witness),
        ..Scope::at(&path.ext)
    };
    // Under a correlation witness the guard is evaluated per
    // thread, like a step predicate (see `apply_predicates`).
    if !witness.is_empty() {
        return exprs
            .iter()
            .all(|e| eval_pred_expr_at(adapter, path.ext.node, e, trace, witness, scope));
    }
    exists_binding(
        adapter,
        path.ext.node,
        &exprs,
        trace,
        &mut Vec::new(),
        scope,
    )
}

/// Fold one alternative's elements over a set of in-flight paths.
/// Steps apply from each path's node and drop successors the path has
/// already visited; nested groups recurse with the outer path's
/// visited set threaded through (a simple path never revisits a node,
/// however deeply the pattern nests).
pub(crate) fn expand_elems(
    adapter: &impl AstAdapter,
    elems: &[PathElem],
    mut paths: Vec<GPath>,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
) -> Vec<GPath> {
    for elem in elems {
        // A mark files a reference to each in-flight path's node.
        if let PathElem::Mark(name) = elem {
            for path in &mut paths {
                path.register.push(mark_reg(adapter, path.ext.node, name.clone()));
            }
            continue;
        }
        let mut next = Vec::new();
        if let PathElem::Step(step) = elem {
            prefetch_hint(adapter, step, paths.iter().map(|p| p.ext.node));
        }
        for path in &paths {
            match elem {
                PathElem::Step(step) => {
                    let mut succs = Vec::new();
                    apply_step(
                        adapter,
                        path.ext.node,
                        step,
                        trace,
                        outer,
                        witness,
                        &path.ext,
                        &path.register,
                        &mut succs,
                    );
                    next.extend(succs.into_iter().filter(|&s| !path.blocks(s)).map(|s| {
                        let edge = arrived_edge(adapter, path.ext.node, step, s);
                        path.with(s, edge)
                    }));
                }
                PathElem::Group(inner) => {
                    next.extend(expand_group(
                        adapter,
                        inner,
                        path.clone(),
                        trace,
                        outer,
                        witness,
                    ));
                }
                PathElem::Mark(_) => unreachable!("handled above"),
                // A breadcrumb: evaluate from the path's node — with
                // the walked edge as `$-` — and push. The path does
                // not move.
                PathElem::Push { name, body } => {
                    // The body sees no marks (as before the frontier
                    // carried them): a bare attachment at the node.
                    let state = GraphExt::at(path.ext.node);
                    let scope = Scope {
                        prov: None,
                        outer,
                        crossing: path.last_edge.as_ref(),
                        witness: (!witness.is_empty()).then_some(witness),
                        ..Scope::at(&state)
                    };
                    let (value, prov) = match body {
                        PushBody::Query(q) => {
                            subcontext_traced(q, adapter, path.ext.node, trace, Some(&scope))
                        }
                        PushBody::Expr(e) => {
                            let (v, o) = capture(|| {
                                operand_value_at(adapter, path.ext.node, e, trace, witness, scope)
                            });
                            (v, Prov::leaf(o))
                        }
                    };
                    let mut p = path.clone();
                    p.register.push(Reg {
                        prov,
                        site: Origin {
                            position: (path.ext.node).into(),
                            command: command_now(),
                        },
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
pub(crate) fn dedup_paths(paths: Vec<GPath>) -> Vec<GPath> {
    let mut kept: Vec<GPath> = Vec::new();
    let mut index: HashMap<_, usize> = HashMap::new();
    for p in paths {
        let key = p.key();
        match index.get(&key) {
            Some(&i) => {
                for (a, b) in kept[i].register.iter_mut().zip(&p.register) {
                    a.prov.origins.union(&b.prov.origins);
                }
            }
            None => {
                index.insert(key, kept.len());
                kept.push(p);
            }
        }
    }
    kept
}

/// Project one node to a scalar value.
pub(crate) fn project(adapter: &impl AstAdapter, node: NodeId, proj: &Projection) -> Value {
    match proj {
        Projection::Property(Some(name)) => property_or_alias(adapter, node, name),
        Projection::Property(None) => adapter.default_value(node).unwrap_or(Value::Null),
        // An unknown core field is refused naming the core fields
        // (draft 164), as an unknown unit is; `:fields` lists them.
        Projection::CoreMeta(key) => core_meta(adapter, node, key).unwrap_or_else(|| {
            crate::value::ambient::record_refusal(format!(
                "':{key}' is no core field; the core fields are {} (':fields' lists them); a property is '::{key}', adapter metadata ':::{key}'",
                crate::graph::navigate::CORE_FIELDS.join(", ")
            ));
            Value::Null
        }),
        Projection::AdapterMeta(key) => adapter.metadata(node, key).unwrap_or(Value::Null),
    }
}

/// Apply one step to a single source node, appending results to `out`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_step(
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    out: &mut Vec<NodeId>,
) {
    match &step.axis {
        Axis::Child => {
            // Literal names take the adapter's name-addressed fast
            // path. The adapter owns the name test there (its
            // contract) — which permits deliberate aliasing, like
            // git revision syntax resolving to a hash-named commit
            // — so only the nameless tests re-run.
            // A transparent link lends its target's children to the
            // explicit step (the descendant walk never asks).
            let host = adapter.child_host(node);
            // The leading equality (`/*[::col = v]`, `/name[::k =
            // v]`) travels in the plan: an adapter that indexes the
            // property answers it, and the engine runs the rest.
            let conjuncts = match step.predicates.first() {
                Some(Predicate::Expr(e)) => indexable_equality(e)
                    .map(|(prop, value)| vec![Conjunct::Equals(prop, value)])
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let plan = StepPlan {
                traits: Vec::new(),
                cap: Default::default(),
                axis: PlanAxis::Child,
                name: match &step.matcher {
                    Matcher::Name(n) => Some(n),
                    _ => None,
                },
                conjuncts,
                limit: None,
                order: None,
            };
            let mut consumed_first = false;
            let selected = adapter.select(host, &plan).map(|sel| {
                consumed_first = sel.applied.first().copied().unwrap_or(false);
                sel.nodes
            });
            let matched: Vec<NodeId> = match (&step.matcher, selected) {
                // A child plan's answer is the adapter's name test;
                // the nameless tests still run.
                (Matcher::Name(_), Some(nodes)) => nodes
                    .into_iter()
                    .map(|(c, _)| c)
                    .filter(|&c| tests_ok(adapter, c, step))
                    .collect(),
                (Matcher::Name(n), None) => adapter
                    .children_named(host, n)
                    .into_iter()
                    .filter(|&c| tests_ok(adapter, c, step))
                    .collect(),
                (_, Some(nodes)) => nodes
                    .into_iter()
                    .map(|(c, _)| c)
                    .filter(|&c| matches_step(adapter, c, step))
                    .collect(),
                (_, None) => adapter
                    .children(host)
                    .into_iter()
                    .filter(|&c| matches_step(adapter, c, step))
                    .collect(),
            };
            let rest;
            let step = if consumed_first {
                rest = Step {
                    predicates: step.predicates[1..].to_vec(),
                    ..step.clone()
                };
                &rest
            } else {
                step
            };
            out.extend(apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                witness,
                state,
                register,
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
        Axis::Descendant(reach) => descendant_step(
            reach, adapter, node, step, trace, outer, witness, state, register, out,
        ),
        Axis::Universal => {
            // The whole arbor: the root (the anchor node) and its
            // descendants in document order, then the nodes outside
            // the tree in the adapter's order; one step, so the
            // tests run on each.
            let mut found: Vec<(NodeId, usize)> = Vec::new();
            if matches_step(adapter, node, step) {
                found.push((node, 0));
            }
            descendants(adapter, node, 1, step, &mut found);
            for n in adapter.non_tree_nodes() {
                if matches_step(adapter, n, step) {
                    found.push((n, 0));
                }
            }
            let found = apply_predicates(
                adapter,
                found,
                step,
                trace,
                outer,
                witness,
                state,
                register,
                |&(n, _)| n,
                |&(n, _)| {
                    Some(EdgeCtx {
                        source: adapter.parent(n).unwrap_or(node),
                        label: CHILD.into(),
                        target: n,
                    })
                },
            );
            out.extend(found.into_iter().map(|(n, _)| n));
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
                witness,
                state,
                register,
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
                witness,
                state,
                register,
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
                witness,
                state,
                register,
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
                witness,
                state,
                register,
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
                witness,
                state,
                register,
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
            let matched: Vec<(String, NodeId)> = select_links(step, &links_of(adapter, node))
                .into_iter()
                .filter(|(_, target)| tests_ok(adapter, *target, step))
                .collect();
            let kept = apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                witness,
                state,
                register,
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
            let matched: Vec<(String, NodeId)> = select_links(step, &backlinks_of(adapter, node))
                .into_iter()
                .filter(|(_, source)| tests_ok(adapter, *source, step))
                .collect();
            let kept = apply_predicates(
                adapter,
                matched,
                step,
                trace,
                outer,
                witness,
                state,
                register,
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
        // `--` — both directions under one matcher: the node's
        // outgoing crosslinks and its backlinks, each edge kept
        // with its true direction so the edge context (`$-`) still
        // reports source and target faithfully.
        Axis::BothLink => both_link_step(
            adapter, node, step, trace, outer, witness, state, register, out,
        ),
        Axis::Resolve {
            property,
            label: hint,
        } => resolve_step(
            property, hint, adapter, node, step, trace, outer, witness, state, register, out,
        ),
        Axis::ReverseResolve {
            label,
            scope,
            property,
        } => reverse_resolve_step(
            label, scope, property, adapter, node, step, trace, outer, witness, state, register,
            out,
        ),
        Axis::ValueMatch {
            property,
            scope,
            target,
        } => value_match_step(
            property, scope, target, adapter, node, step, trace, outer, witness, state, register,
            out,
        ),
    }
}

/// `//` — the descendants, by reach. `apply_step`'s arm.
pub(crate) fn descendant_step(
    reach: &Reach,
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    out: &mut Vec<NodeId>,
) {
    // An adapter that indexes names answers `//name` in one
    // call (its answer is the walk's, by contract); the
    // nameless tests still run here.
    let mut found = Vec::new();
    // An adapter that indexes a property answers
    // `//name[::prop = "lit"]` in one call too (the corpus
    // reading's positional index); the leading equality is
    // then already applied and the rest of the step's
    // predicates run as usual.
    let mut consumed_first = false;
    let indexed = match &step.matcher {
        Matcher::Name(n) => {
            let conjuncts = match step.predicates.first() {
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
            adapter.select(node, &plan).map(|sel| {
                consumed_first = sel.applied.first().copied().unwrap_or(false);
                sel.nodes
            })
        }
        _ => None,
    };
    match indexed {
        Some(v) => found.extend(v.into_iter().filter(|&(n, _)| tests_ok(adapter, n, step))),
        None => descendants(adapter, node, 1, step, &mut found),
    }
    let rest;
    let step = if consumed_first {
        rest = Step {
            predicates: step.predicates[1..].to_vec(),
            ..step.clone()
        };
        &rest
    } else {
        step
    };
    let found = apply_predicates(
        adapter,
        found,
        step,
        trace,
        outer,
        witness,
        state,
        register,
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

/// `<->` — the links in both directions. `apply_step`'s arm.
pub(crate) fn both_link_step(
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    out: &mut Vec<NodeId>,
) {
    let matched: Vec<(String, NodeId, bool)> = select_links(step, &links_of(adapter, node))
        .into_iter()
        .map(|(label, target)| (label, target, true))
        .chain(
            select_links(step, &backlinks_of(adapter, node))
                .into_iter()
                .map(|(label, source)| (label, source, false)),
        )
        .filter(|(_, other, _)| tests_ok(adapter, *other, step))
        .collect();
    let kept = apply_predicates(
        adapter,
        matched,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |(_, n, _)| *n,
        |(label, other, outgoing)| {
            Some(if *outgoing {
                EdgeCtx {
                    source: node,
                    label: label.clone(),
                    target: *other,
                }
            } else {
                EdgeCtx {
                    source: *other,
                    label: label.clone(),
                    target: node,
                }
            })
        },
    );
    out.extend(kept.into_iter().map(|(_, other, _)| other));
}

/// `-->` — follow a reference property to its target. `apply_step`'s arm.
pub(crate) fn resolve_step(
    property: &Option<String>,
    hint: &Option<String>,
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    out: &mut Vec<NodeId>,
) {
    // A property holding a node reference (spec: The Capsa, "Node
    // references") resolves to its node directly: no declared
    // identity is consulted, and a source that defines none still
    // answers.
    if let Some(p) = property
        && let Some(Value::Node(n)) = adapter.property(node, p)
    {
        let target = NodeId(n.id);
        if adapter.has_node(target) && tests_ok(adapter, target, step) {
            out.extend(apply_predicates(
                adapter,
                vec![target],
                step,
                trace,
                outer,
                witness,
                state,
                register,
                |&n| n,
                |&t| {
                    Some(EdgeCtx {
                        source: node,
                        label: p.clone(),
                        target: t,
                    })
                },
            ));
        }
        return;
    }
    // The bare arrow (`//a-->`) asks the adapter which
    // property carries this node's reference; a named
    // property spells it. No property at all resolves
    // nothing — an unreferencing node simply yields empty.
    // A source with no notion of a reference cannot answer:
    // an empty set would read as "this points nowhere".
    if !adapter.resolves_references() {
        record_refusal(
            "resolution follows references, and this source defines none \
                 (no keys, no reference properties): match by value instead — \
                 '::a=>(/scope/*::b)' — or declare the reference in a model \
                 ('ref /scope/*::a --> target;')"
                .into(),
        );
        return;
    }
    let prop = property.clone().or_else(|| adapter.ref_property(node));
    let Some(prop) = prop else { return };
    let property = &prop;
    // A property that declares no target resolves only under
    // a reference label: without one the answer is refused,
    // whatever the data holds.
    if hint.is_none() && adapter.resolve_needs_label(node, property) {
        record_refusal(format!(
            "'::{property}-->' needs a reference label here: the property \
                 declares no target, so name the container it points into — \
                 '::{property}-->target'"
        ));
        return;
    }
    let mut resolved: Vec<NodeId> = adapter
        .resolve(node, property, hint.as_deref())
        .into_iter()
        .collect();
    if resolved.is_empty() {
        // The property may hold an external reference — a
        // document outside the arbor, and perhaps a location
        // within it. The adapter has read both out of its
        // own reference syntax; the engine looks at neither:
        // a mounted target answers with the node its own
        // adapter finds at the location (else its root); an
        // unmounted one is
        // recorded among the run's unresolved external
        // references, for the host's acquisition loop. No
        // IO here, ever.
        if let Some(ext) = adapter.external_ref(node, property, hint.as_deref()) {
            let frag = ext.within;
            match find_tree(
                adapter,
                &ext.document,
                node,
                trace,
                outer,
                witness,
                register,
            ) {
                Ok(root) => {
                    let landed = frag
                        .and_then(|f| adapter.resolve_fragment(root, &f))
                        .unwrap_or(root);
                    resolved.push(landed);
                }
                Err(Some(identity)) => record_ref(identity),
                Err(None) => {}
            }
        }
    }
    let matched: Vec<NodeId> = resolved
        .into_iter()
        .filter(|&t| tests_ok(adapter, t, step))
        .collect();
    out.extend(apply_predicates(
        adapter,
        matched,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |&n| n,
        // A resolution edge is labeled by its relation —
        // html's rel — falling back to the property name.
        |&t| {
            Some(EdgeCtx {
                source: node,
                label: adapter
                    .ref_label(node, property)
                    .unwrap_or_else(|| property.clone()),
                target: t,
            })
        },
    ));
}

/// `<--` — what points here: every node whose property resolves to this one. `apply_step`'s arm.
pub(crate) fn reverse_resolve_step(
    label: &Option<String>,
    scope: &Option<crate::syntax::ast::ScopePath>,
    property: &RefProp,
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    out: &mut Vec<NodeId>,
) {
    // "What points here?" — every node whose property resolves
    // to `node`. A declared reference (a catalog foreign key, a
    // model `ref`) answers through the adapter's reverse index;
    // a scope path bounds the search to the nodes it selects;
    // with neither, the whole arbor is walked, which an adapter
    // must open to (an empty answer could not be told from an
    // absence of referrers), else the query is refused.
    // A model `index SCOPE::prop;` over this very scope names
    // the candidates without the walk; each is still tested
    // below, so the answer is the walk's.
    let indexed = match (scope, property) {
        (Some(sp), RefProp::Named(p)) => {
            adapter.index_referrers(&sp.text(), p, label.as_deref(), node)
        }
        _ => None,
    };
    // With no reference label written, the adapter may name
    // one under which a referrer's property is also resolved
    // when the question is about this node (a database reads
    // a column with no declared key as a key into the node's
    // table). The engine supplies none of its own.
    let implied = label.clone().or_else(|| adapter.reverse_label(node));
    // The candidates: the scope's nodes — found once a run
    // where the scope looks nowhere outward, and narrowed to
    // those known to resolve here when the property is named
    // — or, with no scope, the declared referrers, else the
    // whole arbor where the adapter opens to the walk.
    let narrowed = |memo: &ScopeMemo| match property {
        RefProp::Named(p) => memo_referrers(
            adapter,
            memo,
            p,
            |_| vec![p.clone()],
            label.as_deref(),
            implied.as_deref(),
            node,
        ),
        // The declared reference properties depend on the
        // container the target stands in: one map per
        // container.
        RefProp::Default => memo_referrers(
            adapter,
            memo,
            &format!(
                "\u{0}default:{}",
                adapter.parent(node).map_or(u64::MAX, |p| p.0)
            ),
            |u| adapter.reference_properties(u, node),
            label.as_deref(),
            implied.as_deref(),
            node,
        ),
        // Any property: no implied container (see below).
        RefProp::Any => memo_referrers(
            adapter,
            memo,
            "\u{0}any",
            |u| adapter.property_names(u),
            label.as_deref(),
            None,
            node,
        ),
    };
    let mut memo: Option<std::rc::Rc<ScopeMemo>> = None;
    let candidates: Vec<NodeId> = match scope {
        Some(_) if indexed.is_some() => indexed.unwrap_or_default(),
        Some(sp) => {
            let text = sp.text();
            let walk = || {
                navigate_from(
                    &sp.0,
                    adapter,
                    adapter.root(),
                    trace,
                    outer,
                    witness,
                    register,
                )
            };
            if scope_is_static(&text) {
                let m = scope_memo(adapter, &text, walk);
                let c = narrowed(&m);
                memo = Some(m);
                c
            } else {
                walk()
            }
        }
        None => {
            let named = match property {
                RefProp::Named(p) => Some(p.as_str()),
                _ => None,
            };
            match adapter.reverse_resolve(node, named, label.as_deref()) {
                Some(sources) if *property != RefProp::Any => {
                    let found: Vec<(NodeId, String)> = sources
                        .into_iter()
                        .filter(|&u| tests_ok(adapter, u, step))
                        .map(|u| {
                            let p = named
                                .map(str::to_string)
                                .or_else(|| {
                                    adapter.reference_properties(u, node).into_iter().next()
                                })
                                .unwrap_or_else(|| "ref".to_string());
                            (u, p)
                        })
                        .collect();
                    out.extend(reverse_found(
                        adapter, node, found, step, trace, outer, witness, state, register,
                    ));
                    return;
                }
                _ => {
                    if !adapter.walks_for_references() {
                        record_refusal(
                            "reverse resolution over the whole arbor: this source \
                                 does not open to the walk — give the arrow a scope, \
                                 '<--(/path/*::y)', or declare the reference in a model"
                                .into(),
                        );
                        return;
                    }
                    let m = scope_memo(adapter, "", || whole_arbor(adapter));
                    let c = narrowed(&m);
                    memo = Some(m);
                    c
                }
            }
        }
    };
    // A source with no notion of a reference cannot answer:
    // an empty set would read as "nothing points here".
    if !adapter.resolves_references() {
        record_refusal(
            "reverse resolution follows references, and this source defines \
                 none (no keys, no reference properties): match by value \
                 instead — 't::id=>(/scope/*::y)' — or declare the reference \
                 in a model ('ref /scope/*::y --> target;')"
                .into(),
        );
        return;
    }
    // Under `::*` with no label written the question is
    // settled by declaration, before any property is read:
    // can a reference declared on the scoped nodes — a
    // catalog key, a model `ref`, a reference property the
    // format defines — land in this node's container? If
    // none can, an empty answer would read as an absence of
    // referrers, and the query is refused. The predicates of
    // the scope do not decide it: the same query behaves the
    // same way whatever the data holds.
    let asks_declaration = match property {
        RefProp::Any => label.is_none(),
        // `t<--` and `t<--(SCOPE)` read the reference property
        // the scoped nodes declare: where none does, there is
        // nothing to read.
        RefProp::Default => true,
        RefProp::Named(_) => false,
    };
    if asks_declaration {
        let declares = |u: NodeId| match property {
            RefProp::Default => !adapter.reference_properties(u, node).is_empty(),
            _ => adapter.can_refer_to(u, node) == Some(true),
        };
        let declared = |ns: &[NodeId]| ns.iter().any(|&u| u != node && declares(u));
        // Asked of the whole scope, once a run per container
        // where the scope is memoized.
        let memo_key = (
            *property == RefProp::Any,
            adapter.parent(node).map_or(u64::MAX, |p| p.0),
        );
        let known = memo
            .as_ref()
            .and_then(|m| m.declared.borrow().get(&memo_key).copied());
        let mut ok = match (known, &memo) {
            (Some(k), _) => k,
            (None, Some(m)) => declared(&m.nodes),
            (None, None) => declared(&candidates),
        };
        if !ok
            && known.is_none()
            && let Some(sp) = scope
        {
            let bare = without_predicates(&sp.0);
            let all = navigate_from(
                &bare,
                adapter,
                adapter.root(),
                trace,
                outer,
                witness,
                register,
            );
            ok = declared(&all);
            if !ok && all.is_empty() && !bare.is_empty() {
                // An empty scope has no node to ask: its
                // containers answer where the source keeps a
                // schema (a table with no rows still has its
                // keys); where none says no, the search runs.
                let containers = navigate_from(
                    &bare[..bare.len() - 1],
                    adapter,
                    adapter.root(),
                    trace,
                    outer,
                    witness,
                    register,
                );
                let answers: Vec<Option<bool>> = containers
                    .iter()
                    .map(|&c| adapter.can_refer_to(c, node))
                    .collect();
                ok = answers.contains(&Some(true)) || !answers.contains(&Some(false));
            }
        }
        if let Some(m) = &memo {
            m.declared.borrow_mut().insert(memo_key, ok);
        }
        if !ok {
            let msg = if *property == RefProp::Any {
                let remedy = implied
                    .as_deref()
                    .map(|c| format!(", or write the reference label, '<--{c}(…::*)'"))
                    .unwrap_or_default();
                format!(
                    "reverse resolution through any property ('::*'): nothing \
                         declared on the scoped nodes can refer to this node's \
                         container, so an empty answer would not mean that nothing \
                         points here — name the property, '<--(…::y)'{remedy}"
                )
            } else {
                let place = if scope.is_some() {
                    "the scoped nodes declare"
                } else {
                    "this source declares"
                };
                format!(
                    "reverse resolution with no property named: {place} no \
                         reference property that can point at this node's \
                         container, so there is nothing to read — name the \
                         property, '<--(…::y)'"
                )
            };
            record_refusal(msg);
            return;
        }
    }
    let mut found: Vec<(NodeId, String)> = Vec::new();
    for u in candidates {
        if u == node {
            continue;
        }
        let props: Vec<String> = match property {
            RefProp::Named(p) => vec![p.clone()],
            RefProp::Default => adapter.reference_properties(u, node),
            RefProp::Any => adapter.property_names(u),
        };
        if let Some(p) = props.into_iter().find(|p| {
            // The implied container serves a property the
            // query names; under `::*` it would let any
            // property whose value happens to equal the key
            // count as a reference, so there a property must
            // resolve on its own (or under a written label).
            adapter.resolve(u, p, label.as_deref()) == Some(node)
                || (label.is_none()
                    && *property != RefProp::Any
                    && adapter.resolve(u, p, implied.as_deref()) == Some(node))
        }) && tests_ok(adapter, u, step)
        {
            found.push((u, p));
        }
    }
    out.extend(reverse_found(
        adapter, node, found, step, trace, outer, witness, state, register,
    ));
}

/// `=>` — the value match: the nodes of the scope whose target equals this node's property. `apply_step`'s arm.
pub(crate) fn value_match_step(
    property: &String,
    scope: &Option<crate::syntax::ast::ScopePath>,
    target: &String,
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    out: &mut Vec<NodeId>,
) {
    // The nodes of the scope whose `target` equals this
    // node's `property`: plain equality, no resolution, no key.
    // Unscoped, the whole arbor on the reverse arrow's terms.
    let Some(own) = adapter.property(node, property) else {
        return;
    };
    let candidates: Vec<NodeId> = match scope {
        // A model `index SCOPE::prop;` over this scope hands
        // the nodes holding the value; else the scope is
        // read once a run and its nodes are found by the
        // value's key. Each is still tested below.
        Some(sp) => {
            let text = sp.text();
            match adapter.index_lookup(&text, target, &own) {
                Some(hits) => hits,
                None => {
                    let walk = || {
                        navigate_from(
                            &sp.0,
                            adapter,
                            adapter.root(),
                            trace,
                            outer,
                            witness,
                            register,
                        )
                    };
                    if scope_is_static(&text) {
                        let memo = scope_memo(adapter, &text, walk);
                        memo_holders(adapter, &memo, target, &own)
                    } else {
                        walk()
                    }
                }
            }
        }
        None => {
            if !adapter.walks_for_references() {
                record_refusal(
                    "value match over the whole arbor: this source does not \
                         open to the walk — give the match a scope, '=>(/path/*::b)'"
                        .into(),
                );
                return;
            }
            let memo = scope_memo(adapter, "", || whole_arbor(adapter));
            memo_holders(adapter, &memo, target, &own)
        }
    };
    let scale = |e: &str| adapter.unit_scale(e);
    let matched: Vec<NodeId> = candidates
        .into_iter()
        .filter(|&u| {
            adapter
                .property(u, target)
                .is_some_and(|v| value_eq(&own, &v, &scale))
                && tests_ok(adapter, u, step)
        })
        .collect();
    out.extend(apply_predicates(
        adapter,
        matched,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |&n| n,
        |&u| {
            Some(EdgeCtx {
                source: node,
                label: format!("{property}={target}"),
                target: u,
            })
        },
    ));
}

/// The referrers a reverse resolution found, through the step's
/// predicates, each arriving by the stored edge (the referrer's
/// property points at the current node).
#[allow(clippy::too_many_arguments)]
pub(crate) fn reverse_found(
    adapter: &impl AstAdapter,
    node: NodeId,
    found: Vec<(NodeId, String)>,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
) -> Vec<NodeId> {
    apply_predicates(
        adapter,
        found,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |&(n, _)| n,
        |&(source, ref p)| {
            Some(EdgeCtx {
                source,
                label: adapter.ref_label(source, p).unwrap_or_else(|| p.clone()),
                target: node,
            })
        },
    )
    .into_iter()
    .map(|(n, _)| n)
    .collect()
}

/// Tell the adapter which crosslinks a hop is about to follow from
/// which nodes — a batching hint, no semantics.
pub(crate) fn prefetch_hint(
    adapter: &impl AstAdapter,
    step: &Step,
    nodes: impl Iterator<Item = NodeId>,
) {
    let dirs: &[crate::graph::adapter::LinkDir] = match step.axis {
        Axis::OutLink => &[crate::graph::adapter::LinkDir::Out],
        Axis::InLink => &[crate::graph::adapter::LinkDir::In],
        Axis::BothLink => &[
            crate::graph::adapter::LinkDir::Out,
            crate::graph::adapter::LinkDir::In,
        ],
        // A resolution with no label follows the declared keys —
        // the outgoing crosslinks — so the same hint serves it.
        Axis::Resolve { label: None, .. } => &[crate::graph::adapter::LinkDir::Out],
        _ => return,
    };
    let nodes: Vec<NodeId> = nodes.collect();
    if nodes.len() < 2 {
        return;
    }
    for d in dirs {
        adapter.prefetch_links(&nodes, *d);
    }
}

/// Collect `node` and all of its descendants (the whole subtree).
pub(crate) fn collect_subtree(adapter: &impl AstAdapter, node: NodeId, out: &mut Vec<NodeId>) {
    // An explicit stack, so a tree as deep as the source allows
    // (XML nested hundreds of thousands of levels) walks within
    // the thread's stack. Children are pushed reversed to keep
    // document order.
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        out.push(n);
        stack.extend(adapter.children(n).into_iter().rev());
    }
}

/// Collect proper descendants of `node` matching `step`'s tests (name,
/// traits, leaf — not predicates), in document order, tagged with
/// their depth (direct children are depth 1).
pub(crate) fn descendants(
    adapter: &impl AstAdapter,
    node: NodeId,
    depth: usize,
    step: &Step,
    out: &mut Vec<(NodeId, usize)>,
) {
    // An explicit stack (see `collect_subtree`): depth-first in
    // document order, no recursion per level.
    let mut stack: Vec<(NodeId, usize)> = adapter
        .children(node)
        .into_iter()
        .rev()
        .map(|c| (c, depth))
        .collect();
    while let Some((child, d)) = stack.pop() {
        if matches_step(adapter, child, step) {
            out.push((child, d));
        }
        stack.extend(
            adapter
                .children(child)
                .into_iter()
                .rev()
                .map(|c| (c, d + 1)),
        );
    }
}

/// Whether `node` passes `step`'s tests: the name matcher, the trait
/// filters, and the leaf anchor. Predicates are *not* checked here —
/// they apply to the hop's collected result list (`apply_predicates`).
/// `[::prop = "text"]` — the one predicate shape an adapter's
/// property index can answer (a named property of the node itself
/// against a text literal); anything else keeps the walk.
/// A leading predicate lowered for pricing: `::prop OP literal` on
/// the current node, any comparison, any literal. Text equalities
/// are [`indexable_equality`]'s (an index answers them); the rest
/// are comparisons a listing or a store's statistics may price.
pub(crate) fn indexable_comparison(
    e: &PredExpr,
) -> Option<(&str, crate::graph::adapter::Cmp, &Value)> {
    use crate::graph::adapter::Cmp;
    if let Some((p, v)) = indexable_equality(e) {
        return Some((p, Cmp::Eq, v));
    }
    let PredExpr::Compare(l, op, r) = e else {
        return None;
    };
    let Operand::Rel {
        steps,
        projection: Some(Projection::Property(Some(prop))),
        anchor: Anchor::Current,
    } = l
    else {
        return None;
    };
    if !steps.is_empty() {
        return None;
    }
    let Operand::Lit(v) = r else {
        return None;
    };
    if matches!(v, Value::Null | Value::List(_)) {
        return None;
    }
    let cmp = match op {
        CmpOp::Eq => Cmp::Eq,
        CmpOp::Ne => Cmp::Ne,
        CmpOp::Lt => Cmp::Lt,
        CmpOp::Le => Cmp::Le,
        CmpOp::Gt => Cmp::Gt,
        CmpOp::Ge => Cmp::Ge,
        CmpOp::Contains => Cmp::Contains,
        _ => return None,
    };
    Some((prop.as_str(), cmp, v))
}

/// The step's trait filters as literal names, when every clause
/// is one plain name. A negation or a segment wildcard (`*`,
/// `tag:.`) keeps the whole step on the walk, where
/// [`trait_clauses_ok`] tests it.
pub(crate) fn literal_traits(step: &Step) -> Vec<&str> {
    let mut out = Vec::new();
    for clause in &step.traits {
        match clause.alts.as_slice() {
            [one] if !one.starts_with('!') && !crate::syntax::ast::trait_has_wildcard(one) => {
                out.push(one.as_str())
            }
            _ => return Vec::new(),
        }
    }
    out
}

pub(crate) fn indexable_equality(e: &PredExpr) -> Option<(&str, &Value)> {
    let PredExpr::Compare(l, CmpOp::Eq, r) = e else {
        return None;
    };
    let Operand::Rel {
        steps,
        projection: Some(Projection::Property(Some(prop))),
        anchor: Anchor::Current,
    } = l
    else {
        return None;
    };
    if !steps.is_empty() {
        return None;
    }
    match r {
        Operand::Lit(v @ Value::Str(_)) => Some((prop.as_str(), v)),
        _ => None,
    }
}

pub(crate) fn matches_step(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    matches_name(adapter, node, &step.matcher) && tests_ok(adapter, node, step)
}

/// The nameless part of `matches_step` (trait filters and the leaf
/// anchor), for crosslink axes where the matcher applies to the edge
/// label instead of the node name.
pub(crate) fn tests_ok(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    if !traits_ok(adapter, node, step) {
        return false;
    }
    !step.leaf || adapter.children(node).is_empty()
}

/// Whether `node` would match `elem` as a step of a path, decided
/// at the node and nowhere else: the name matcher, the trait
/// clauses, the leaf anchor and the expression predicates, with no
/// hop performed and no context beyond the node (a predicate may
/// still look around from it). A name-alternation group matches
/// when any alternative does and the group's own clauses hold. A
/// model's declared trait is this test, so the engine never walks
/// the arbor to answer `<declared>`.
pub fn node_matches_elem(adapter: &impl AstAdapter, node: NodeId, elem: &PathElem) -> bool {
    let trace = Correlation::default();
    let edge = |n: &NodeId| {
        Some(EdgeCtx {
            source: adapter.parent(*n).unwrap_or(*n),
            label: CHILD.into(),
            target: *n,
        })
    };
    match elem {
        PathElem::Step(step) => {
            matches_step(adapter, node, step)
                && !apply_predicates(
                    adapter,
                    vec![node],
                    step,
                    &trace,
                    None,
                    &[],
                    &GraphExt::at(adapter.root()),
                    &[],
                    |&n| n,
                    edge,
                )
                .is_empty()
        }
        PathElem::Group(g) => {
            let any_alt = g.alts.iter().any(|alt| match alt.as_slice() {
                [PathElem::Step(s)] => matches_step(adapter, node, s),
                _ => false,
            });
            if !any_alt {
                return false;
            }
            // The group's own clauses, through a step that carries
            // them (positional predicates are refused upstream).
            let own = Step {
                axis: Axis::Universal,
                matcher: Matcher::Any,
                labels: Vec::new(),
                traits: g.traits.clone(),
                predicates: g.predicates.clone(),
                leaf: false,
            };
            tests_ok(adapter, node, &own)
                && !apply_predicates(
                    adapter,
                    vec![node],
                    &own,
                    &trace,
                    None,
                    &[],
                    &GraphExt::at(adapter.root()),
                    &[],
                    |&n| n,
                    edge,
                )
                .is_empty()
        }
        _ => false,
    }
}

/// Apply `step`'s predicates to one hop's result list (the tested
/// matches from a single source node, in axis order), sequentially:
///
/// - An index predicate `[n]` keeps the `n`-th element of the list
///   as filtered so far (`[-1]` the last) — position among the hop's
///   results, not sibling position (that is `[:index = n]`). Out
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
pub(crate) fn apply_predicates<T>(
    adapter: &impl AstAdapter,
    mut items: Vec<T>,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
    node_of: impl Fn(&T) -> NodeId,
    edge_of: impl Fn(&T) -> Option<EdgeCtx>,
) -> Vec<T> {
    // The walk's state the predicates evaluate in: the path's marks
    // and witness, built once per call (a navigation scope stands on
    // no thread; its node is the served one, passed beside it).
    let mut i = 0;
    while i < step.predicates.len() {
        match &step.predicates[i] {
            pred @ (Predicate::Index(_) | Predicate::Range(_, _) | Predicate::Positions(_)) => {
                items = positional(items, pred);
                i += 1;
            }
            // A computed bracket reads no node: its value, read once
            // in the walk's scope, is the position or the position
            // list; any other value is a condition on every item.
            Predicate::Computed(op) => {
                i += 1;
                let Some(first) = items.first() else { continue };
                let scope = Scope {
                    prov: None,
                    outer,
                    register,
                    crossing: None,
                    witness: (!witness.is_empty()).then_some(witness),
                    ..Scope::at(state)
                };
                let value = super::operand::operand_value_at(
                    adapter,
                    node_of(first),
                    op,
                    trace,
                    witness,
                    scope,
                );
                match crate::pipeline::positions_of(&value) {
                    Some(pred) => items = positional(items, &pred),
                    None => {
                        let truthy = PredExpr::Truthy(op.clone());
                        items.retain(|item| {
                            let scope = Scope {
                                prov: None,
                                outer,
                                register,
                                crossing: None,
                                witness: (!witness.is_empty()).then_some(witness),
                                ..Scope::at(state)
                            };
                            eval_pred_expr_at(adapter, node_of(item), &truthy, trace, witness, scope)
                        });
                    }
                }
            }
            Predicate::Expr(_) => {
                let mut run: Vec<&PredExpr> = Vec::new();
                while let Some(Predicate::Expr(e)) = step.predicates.get(i) {
                    run.push(e);
                    i += 1;
                }
                items.retain(|item| {
                    let edge = edge_of(item);
                    // Navigation predicates have no capsa of
                    // their own; `$$…` still reaches the
                    // invoking subcontext's capsa. Edge hops
                    // define `$-` for their own predicates.
                    // The path's own capsa: its breadcrumb register
                    // (seeded from the invoking capsa inside a body —
                    // the shared capsa) and its marks; `_` is the
                    // invoking position.
                    let scope = Scope {
                        prov: None,
                        outer,
                        register,
                        crossing: edge.as_ref(),
                        witness: (!witness.is_empty()).then_some(witness),
                        ..Scope::at(&state)
                    };
                    // Under a correlation witness — a pipeline
                    // command's operand path, or a nested path in
                    // an ON clause — `$*k` is THIS thread's bound
                    // node, and the predicates are evaluated
                    // under it, null-propagating, exactly as the
                    // command's own filter is. Re-searching the
                    // trace here would admit a node under
                    // whichever tuple happened to fit — the same
                    // answer for every thread.
                    if !witness.is_empty() {
                        return run.iter().all(|e| {
                            eval_pred_expr_at(adapter, node_of(item), e, trace, witness, scope)
                        });
                    }
                    // No witness yet (a driver path, a body's own
                    // navigation): the existential over the trace.
                    exists_binding(adapter, node_of(item), &run, trace, &mut Vec::new(), scope)
                });
            }
        }
    }
    items
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
pub(crate) fn exists_binding(
    adapter: &impl AstAdapter,
    node: NodeId,
    exprs: &[&PredExpr],
    trace: &Trace,
    bound: &mut Vec<Option<NodeId>>,
    scope: Scope<'_, GraphExt>,
) -> bool {
    if bound.len() == trace.contexts.len() {
        let hit = exprs.iter().all(|e| {
            mentions_null_ctx(e, bound) || eval_pred_expr_at(adapter, node, e, trace, bound, scope)
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

/// Whether a crosslink `label` satisfies `matcher`. `Any` matches any
/// label.
/// Whether a predicate expression references a trace context that is
/// currently bound to null — the exprs an outer join's null binding
/// makes vacuous. A `$*k` reference counts directly or through a
/// path's nested step predicates (`$*1/sub[$*2::x]`, `./sub[$*1::k
/// = 1]`); a pipe tail's commands and a push body's query are
/// contexts of their own.
pub(crate) fn mentions_null_ctx(e: &PredExpr, bound: &[Option<NodeId>]) -> bool {
    struct NullCtx<'a> {
        bound: &'a [Option<NodeId>],
        found: bool,
    }
    impl visit::Visitor for NullCtx<'_> {
        fn operand(&mut self, o: &Operand) {
            match o {
                Operand::Ctx { index: Some(k), .. }
                    if matches!(self.bound.get(k.saturating_sub(1)), Some(None)) =>
                {
                    self.found = true;
                }
                Operand::Piped { expr, .. } => self.operand(expr),
                _ => visit::walk_operand(self, o),
            }
        }
        fn elem(&mut self, e: &PathElem) {
            if !matches!(
                e,
                PathElem::Push {
                    body: PushBody::Query(_),
                    ..
                }
            ) {
                visit::walk_elem(self, e);
            }
        }
    }
    let mut v = NullCtx {
        bound,
        found: false,
    };
    v.pred(e);
    v.found
}

pub(crate) fn matches_label(matcher: &Matcher, label: &str) -> bool {
    matches!(matcher, Matcher::Any) || matcher.matches(label)
}

/// The crosslinks among `pairs` (label, other node) a step keeps.
/// By its matcher, one per edge; by its label selection
/// (`->(a && !b)`), one per other node, the selection reading the
/// labels the crosslinks to that node carry together (spec: Label
/// Selections) — the edge kept carries the first of them.
pub(crate) fn select_links(step: &Step, pairs: &[(String, NodeId)]) -> Vec<(String, NodeId)> {
    if step.labels.is_empty() {
        return pairs
            .iter()
            .filter(|(l, _)| matches_label(&step.matcher, l))
            .cloned()
            .collect();
    }
    let mut grouped: Vec<(NodeId, Vec<String>)> = Vec::new();
    for (l, n) in pairs {
        match grouped.iter_mut().find(|(m, _)| m == n) {
            Some((_, ls)) => ls.push(l.clone()),
            None => grouped.push((*n, vec![l.clone()])),
        }
    }
    grouped
        .into_iter()
        .filter(|(_, ls)| step.labels.iter().all(|c| c.matches(ls)))
        .map(|(n, mut ls)| (ls.swap_remove(0), n))
        .collect()
}

/// Whether `node` satisfies all of `step`'s `<...>` trait clauses.
pub(crate) fn traits_ok(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    trait_clauses_ok(adapter, node, &step.traits)
}

/// Every `<...>` clause holds on `node` (a step's, or a quantified
/// group's).
pub(crate) fn trait_clauses_ok(
    adapter: &impl AstAdapter,
    node: NodeId,
    clauses: &[crate::syntax::ast::TraitClause],
) -> bool {
    if clauses.is_empty() {
        return true;
    }
    let traits = adapter.traits(node);
    clauses
        .iter()
        .all(|c| c.matches_with(&traits, |name| adapter.has_trait(node, name)))
}
