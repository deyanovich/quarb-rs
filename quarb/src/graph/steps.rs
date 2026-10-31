//! Part of the executor: see `exec/mod.rs`.

use super::*;
use crate::diagnostic::{FRONTIER_BOUND, QUANTIFIER_BOUND, WALK};
use crate::graph::adapter::{Conjunct, PlanAxis, StepPlan, reference_label};
use crate::value::ambient::{record_debug, record_refusal_coded};

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
    node_level: bool,
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
            walk: None,
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
        // Every thread forks on its own (spec: Expansion paths): two
        // threads merge only when no later read could tell them
        // apart — the same node, the same arriving edge (its
        // identity: parallel edges and different labels stay apart),
        // the same capsa and marks. A hop from two threads to one
        // node is two threads, as a join's rows are; a walk's exit
        // is one thread per acyclic walk.
        let mut next: Vec<(Option<usize>, GraphExt, Vec<Reg>)> = Vec::new();
        if let PathElem::Step(step) = elem {
            prefetch_hint(adapter, step, ctx.iter().map(|c| c.0.node));
        }
        for (i, (ext, register)) in ctx.iter().enumerate() {
            let node = ext.node;
            match elem {
                PathElem::Step(step) => {
                    // a link hop hands its edges over: one thread
                    // per edge (two rows on one leg, a node linked
                    // both ways)
                    let succs: Vec<(NodeId, Option<EdgeCtx>)> = if let Some(edges) =
                        link_edges(adapter, node, step, trace, outer, witness, ext, register)
                    {
                        edges.into_iter().map(|(s, e)| (s, Some(e))).collect()
                    } else {
                        let mut succs = Vec::new();
                        apply_step(
                            adapter, node, step, trace, outer, witness, ext, register, &mut succs,
                        );
                        succs
                            .into_iter()
                            .map(|s| (s, arrived_edge(adapter, node, step, s)))
                            .collect()
                    };
                    next.extend(succs.into_iter().map(|(s, edge)| {
                        let arrived = edge.into_iter().collect();
                        (
                            Some(i),
                            GraphExt {
                                node: s,
                                bindings: Vec::new(),
                                arrived,
                                unplaced: false,
                                walk: None,
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
                            walk: None,
                        },
                        visited: vec![node],
                        register: register.clone(),
                        last_edge: None,
                        set_walk: true,
                        node_level,
                        hops: 0,
                        total: 0.0,
                    };
                    next.extend(
                        expand_group(adapter, group, anchor, trace, outer, witness)
                            .into_iter()
                            .map(|p| {
                                let arrived = p.last_edge.clone().into_iter().collect();
                                // the walk's facts ride the exit; one
                                // thread per walk, none merged
                                (None, GraphExt { arrived, ..p.ext }, p.register)
                            }),
                    );
                }
                PathElem::Mark(_) => unreachable!("handled above"),
                PathElem::Push { .. } => {}
            }
        }
        let mut merged: Vec<(GraphExt, Vec<Reg>)> = Vec::new();
        type Key = (
            usize,
            NodeId,
            Option<(NodeId, String, NodeId, Option<NodeId>)>,
            Vec<(Option<String>, String)>,
        );
        let mut index: HashMap<Key, usize> = HashMap::new();
        for (from, ext, r) in next {
            let Some(from) = from else {
                merged.push((ext, r));
                continue;
            };
            let edge = ext
                .arrived
                .first()
                .map(|e| (e.source, e.label.clone(), e.target, e.row));
            match index.entry((from, ext.node, edge, reg_key(&r))) {
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
    navigate_from_level(
        elems,
        adapter,
        start,
        trace,
        outer,
        witness,
        seed_register,
        false,
    )
}

/// `navigate_from` with the consumer's word on what it reads:
/// `node_level` when nodes alone (an existence test), so a walk may
/// expand by node where that is exact (spec: Expansion paths).
#[allow(clippy::too_many_arguments)]
pub(crate) fn navigate_from_level(
    elems: &[PathElem],
    adapter: &impl AstAdapter,
    start: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    seed_register: &[Reg],
    node_level: bool,
) -> Vec<NodeId> {
    // the top-level fold (correlate::dedup_merge): one thread per
    // (node, arrival, capsa) — a tree axis reaching one node from
    // several contexts is that node once, as the top-level path
    // answers; a walk's facts keep every walk apart, so a walk inside
    // an operand answers one value per walk (spec: Expansion paths)
    let mut seen = HashSet::new();
    navigate_paths(
        elems,
        adapter,
        start,
        trace,
        outer,
        witness,
        seed_register,
        node_level,
    )
    .into_iter()
    .filter(|(e, r)| seen.insert((e.node, arrival_key(e), reg_key(r))))
    .map(|(e, _)| e.node)
    .collect()
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
    /// Whether the path belongs to a set walk (spec: Expansion
    /// paths): one that reaches each node once, at the fewest
    /// rounds, and so keeps no history (`expand_group`) — where an
    /// exact walk's path is one acyclic walk per history.
    pub(crate) set_walk: bool,
    /// Whether what consumes the walk reads nodes alone (`@|
    /// collapse`, `nodes`, `edges`, `pagerank`, a marker): the
    /// expansion may then go by node and round where that is exact
    /// — under the acyclic promise, or for a plain open walk (spec:
    /// Expansion paths, Acyclic links).
    pub(crate) node_level: bool,
    /// The hops the walk has taken to reach this path's node.
    pub(crate) hops: usize,
    /// Under a budget, the weight summed so far, in the budget's base
    /// unit (spec: The budget quantifier).
    pub(crate) total: f64,
}

/// A budget quantifier resolved under the run's units: the floor
/// and the ceiling in the base unit, the base unit's name, and the
/// bound's written unit with its scale (base per written), for the
/// total a reader sees.
#[derive(Clone, Debug)]
pub(crate) struct BudgetBounds {
    pub(crate) floor: f64,
    pub(crate) ceiling: Option<f64>,
    pub(crate) base: String,
    pub(crate) unit: String,
    pub(crate) scale: f64,
    pub(crate) written: String,
}

impl BudgetBounds {
    /// Resolve a budget's bounds (the floor, the ceiling, as
    /// written) under the run's units; a bound that is no quantity,
    /// or two bounds of two dimensions, refuse by name.
    pub(crate) fn resolve(b: &crate::syntax::ast::Budget) -> Result<BudgetBounds, String> {
        let read = |text: &str| -> Result<(f64, String, f64, String), String> {
            crate::value::quantity::parse_unit_text(&text.replace('_', " ")).ok_or_else(|| {
                if text.trim().parse::<f64>().is_ok() {
                    format!(
                        "one dimension per quantifier: '{text}' counts hops where the other \
                         bound measures a quantity; a budget's bounds are quantities of one \
                         dimension ('{{;100_km}}'), a count of hops is '{{1;5}}'"
                    )
                } else {
                    format!(
                        "the budget's bound '{text}' is no quantity: a budget bounds a sum of an \
                         edge weight, '{{;100_km}}', '{{2_h;}}' — a count of hops is '{{;5}}'"
                    )
                }
            })
        };
        let floor = b.floor.as_deref().map(read).transpose()?;
        let ceiling = b.ceiling.as_deref().map(read).transpose()?;
        let (anchor, written) = match (&ceiling, &floor) {
            (Some(c), _) => (c.clone(), b.ceiling.clone().unwrap_or_default()),
            (None, Some(f)) => (f.clone(), b.floor.clone().unwrap_or_default()),
            (None, None) => return Err("a budget needs a floor or a ceiling".into()),
        };
        if let (Some(c), Some(f)) = (&ceiling, &floor)
            && c.1 != f.1
        {
            return Err(format!(
                "one dimension per quantifier: the floor '{}' measures {} and the ceiling '{}' {}",
                b.floor.as_deref().unwrap_or(""),
                f.1,
                b.ceiling.as_deref().unwrap_or(""),
                c.1
            ));
        }
        let (bv, base, wmag, wunit) = anchor;
        Ok(BudgetBounds {
            floor: floor.map(|f| f.0).unwrap_or(0.0),
            ceiling: ceiling.map(|c| c.0),
            base,
            unit: wunit,
            scale: if wmag != 0.0 { bv / wmag } else { 1.0 },
            written,
        })
    }

    /// The facts of a path under this budget: its total in the base
    /// unit and as the budget wrote its unit.
    pub(crate) fn facts(&self, path: &GPath) -> WalkFacts {
        WalkFacts {
            round: path.hops,
            hops: path.hops,
            total: Some((
                path.total,
                self.base.clone(),
                path.total / self.scale,
                self.unit.clone(),
            )),
            trail: path.trail(),
        }
    }

    /// The weight an edge carries for this budget: the one property
    /// of the budget's dimension, read as a quantity under the run's
    /// units. `Ok(None)` is an edge without the property — not
    /// crossed; two properties of the dimension refuse naming both.
    pub(crate) fn weight(
        &self,
        adapter: &impl AstAdapter,
        edge: &EdgeCtx,
    ) -> Result<Option<f64>, String> {
        let mut found: Vec<(String, f64)> = Vec::new();
        for name in edge.property_names(adapter) {
            let Some(v) = edge.property(adapter, &name) else {
                continue;
            };
            let q = match &v {
                Value::Quantity { value, base, .. } => Some((*value, base.clone())),
                Value::Str(s) => {
                    crate::value::quantity::parse_unit_text(s).map(|(bv, b, _, _)| (bv, b))
                }
                _ => None,
            };
            if let Some((bv, b)) = q
                && b == self.base
            {
                found.push((name, bv));
            }
        }
        match found.as_slice() {
            [] => Ok(None),
            [(_, w)] => Ok(Some(*w)),
            many => Err(format!(
                "two properties of one dimension on the edge '{}': {} — a budget sums one; name \
                 the weight in the model or the query",
                edge.label,
                many.iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )),
        }
    }
}

impl GPath {
    fn blocks(&self, node: NodeId) -> bool {
        self.visited.binary_search(&node).is_ok()
    }

    /// The path extended by one hop to `node`, along `edge`.
    fn with(&self, node: NodeId, edge: Option<EdgeCtx>) -> GPath {
        let mut visited = self.visited.clone();
        if !self.set_walk
            && let Err(i) = visited.binary_search(&node)
        {
            visited.insert(i, node);
        }
        GPath {
            ext: GraphExt {
                node,
                bindings: Vec::new(),
                arrived: Vec::new(),
                unplaced: false,
                walk: None,
            },
            visited,
            register: self.register.clone(),
            last_edge: edge,
            set_walk: self.set_walk,
            node_level: self.node_level,
            hops: self.hops + 1,
            total: self.total,
        }
    }

    /// The walk's facts at this path for a reader without a budget
    /// (`$-:round`, `$-:hops`; `$-:total` null).
    fn facts(&self) -> WalkFacts {
        WalkFacts {
            round: self.hops,
            hops: self.hops,
            total: None,
            trail: self.trail(),
        }
    }

    /// A digest of the path's history: the nodes it crossed.
    fn trail(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.visited.hash(&mut h);
        h.finish()
    }

    /// The dedup key: node, ground covered, breadcrumbs made, and
    /// the arrived-by edge (it feeds future pushes).
    fn key(
        &self,
    ) -> (
        NodeId,
        Vec<NodeId>,
        Vec<(Option<String>, String)>,
        Option<(NodeId, String, NodeId, Option<NodeId>)>,
    ) {
        (
            self.ext.node,
            self.visited.clone(),
            reg_key(&self.register),
            self.last_edge
                .as_ref()
                .map(|e| (e.source, e.label.clone(), e.target, e.row)),
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
            row: None,
        },
        Axis::Parent => EdgeCtx {
            source: from,
            label: PARENT.into(),
            target: succ,
            row: None,
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
                row: None,
            }
        }
        Axis::NextSibling | Axis::FollowingSiblings(_) => EdgeCtx {
            source: from,
            label: NEXT.into(),
            target: succ,
            row: None,
        },
        Axis::PrevSibling | Axis::PrecedingSiblings(_) => EdgeCtx {
            source: from,
            label: PREV.into(),
            target: succ,
            row: None,
        },
        Axis::OutLink => {
            let (label, row) = select_links(step, &links_of(adapter, from), |l, _| {
                adapter.link_consumed(from, l)
            })
            .into_iter()
            .find(|(_, t, _)| *t == succ)
            .map(|(l, _, r)| (l, r))?;
            EdgeCtx {
                source: from,
                label,
                target: succ,
                row,
            }
        }
        Axis::InLink => {
            let (label, row) = select_links(step, &backlinks_of(adapter, from), |l, s| {
                adapter.link_consumed(s, l)
            })
            .into_iter()
            .find(|(_, s, _)| *s == succ)
            .map(|(l, _, r)| (l, r))?;
            EdgeCtx {
                source: succ,
                label,
                target: from,
                row,
            }
        }
        // The headless arrow: the edge context still records the
        // edge's true direction — the walk just declined to assert
        // it. Outgoing is tried first.
        Axis::BothLink => {
            if let Some((label, row)) = select_links(step, &links_of(adapter, from), |l, _| {
                adapter.link_consumed(from, l)
            })
            .into_iter()
            .find(|(_, t, _)| *t == succ)
            .map(|(l, _, r)| (l, r))
            {
                EdgeCtx {
                    source: from,
                    label,
                    target: succ,
                    row,
                }
            } else {
                let (label, row) = select_links(step, &backlinks_of(adapter, from), |l, s| {
                    adapter.link_consumed(s, l)
                })
                .into_iter()
                .find(|(_, s, _)| *s == succ)
                .map(|(l, _, r)| (l, r))?;
                EdgeCtx {
                    source: succ,
                    label,
                    target: from,
                    row,
                }
            }
        }
        // The navop star: the edge of `from` that reaches `succ`
        // (the matching label first, as `link_property` reads it).
        // A deep walk's last hop is not recorded: the edge into
        // `succ` from its parent, else its first backlink.
        Axis::Any | Axis::AnyDeep(_) => {
            if let Some((_, e)) = incident_edges(adapter, from, step)
                .into_iter()
                .find(|(n, _)| *n == succ)
            {
                return Some(e);
            }
            if let Some(p) = adapter.parent(succ) {
                return Some(EdgeCtx {
                    source: p,
                    label: CHILD.into(),
                    target: succ,
                    row: None,
                });
            }
            let (label, s, row) = backlinks_of(adapter, succ).first().cloned()?;
            EdgeCtx {
                source: s,
                label,
                target: succ,
                row,
            }
        }
        Axis::Resolve { property, .. } => EdgeCtx {
            source: from,
            label: property.clone().unwrap_or_else(|| "ref".to_string()),
            target: succ,
            row: None,
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
            row: None,
        },
        Axis::ValueMatch {
            property, target, ..
        } => EdgeCtx {
            source: from,
            label: format!("{property}={target}"),
            target: succ,
            row: None,
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
            record_refusal_coded(
                QUANTIFIER_BOUND,
                "the quantifier",
                format!(
                    "the pattern's explicit ceiling ({n}) exceeds the \
                     quantifier bound ({n_max}); raise --quantifier-bound"
                ),
            );
            return Vec::new();
        }
    } else if group.quant.min > n_max {
        record_refusal_coded(
            QUANTIFIER_BOUND,
            "the quantifier",
            format!(
                "the pattern's minimum ({}) exceeds the quantifier bound \
                 ({n_max}); raise --quantifier-bound",
                group.quant.min
            ),
        );
        return Vec::new();
    }
    let f_max = frontier_bound(adapter);
    // A budget (spec: The budget quantifier): the bounds resolved
    // under the run's units; a budgeted walk is exact.
    let budget = match &group.quant.budget {
        Some(b) => match BudgetBounds::resolve(b) {
            Ok(bounds) => Some(bounds),
            Err(e) => {
                record_refusal_coded(crate::diagnostic::REFUSED, "the quantifier", e);
                return Vec::new();
            }
        },
        None => None,
    };
    // The SET WALK (spec: Expansion paths): a group with no push and
    // no mark, under an open quantifier whose floor is at most one,
    // for all matches or the proximal reach — the forms whose answer
    // is a node set the distance expansion gives exactly. It reaches
    // each node once, at the fewest rounds, and runs to its fixpoint:
    // the graph bounds its rounds, not the quantifier bound. Every
    // other form — a ceiling, a floor above one, the distal reach —
    // and every recording walk is an EXACT WALK: one acyclic walk
    // per history, under the quantifier bound. An exact group inside
    // a set walk records its own ground and hands set paths back; a
    // group inside an exact walk is exact, however deep.
    // Every walk is answered (spec: Expansion paths): the set walk
    // stays an internal expansion where the answer cannot tell, the
    // proximal reach alone — one thread per node at its first round.
    // What consumes the walk reads nodes alone — a marker selects
    // per node, a collapsing reducer or a graph table follows the
    // branch — so a node-level expansion is exact: the plain open
    // walk by node (every node reachable at all is reachable by an
    // acyclic walk), the exact forms under the acyclic promise.
    let node_level = anchor.node_level || group.reach != Reach::All;
    let set_walk = anchor.set_walk
        && group_is_plain(group)
        && set_form(group)
        && (group.reach == Reach::Proximal || (group.reach == Reach::All && node_level))
        && budget.is_none();
    let anchor_was_set = anchor.set_walk;
    let anchor_ground = anchor.visited.clone();
    // The acyclic promise (spec: Model Files, Acyclic links): every
    // hop of the group over a link promised acyclic. A set walk
    // keeps no history, so the promise has nothing to spare it.
    // Over link hops alone the promise is read from the edges the
    // walk crosses, round by round: it holds while every crossed
    // label is promised (an exact label resolves it before the
    // first round, a wildcard after it) and lapses at the first
    // unpromised edge, the histories kept apart from there on.
    let links_only = !set_walk && group_walks_links(group);
    let promise: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(if links_only {
        group_promise(adapter, group)
    } else {
        None
    });
    let alive = std::cell::Cell::new(links_only);
    // The promise changes no answer (spec: Acyclic links): it merges
    // the histories only where nothing read could tell them apart —
    // a node-level reading, and no budget (the totals would tell);
    // its cycle refusal fires on every form, the budgeted too.
    let merge_ok = node_level && budget.is_none();
    let mut anchor = anchor;
    anchor.set_walk = set_walk;
    let hi = if set_walk {
        usize::MAX
    } else {
        group.quant.max.unwrap_or(n_max)
    };
    // The group's predicates filter matches BEFORE reach — the
    // walk continues from the full frontier, but only survivors
    // are candidates. `(...)+[P]?` is therefore "the proximal
    // satisfying P": a shortest-path search.
    let admits = |p: &GPath| {
        group_admits(adapter, group, p, trace, outer, witness, budget.as_ref())
            && budget.as_ref().is_none_or(|b| p.total >= b.floor)
    };
    // The set walk's frontier: a position already reached — the
    // anchor at round 0 — is not expanded again, so the frontier is
    // a set of positions (times capsae), never of histories, and the
    // walk ends on its own when a round reaches nothing new. The
    // exact walk keeps one path per history (`GPath::blocks`).
    let mut seen: HashSet<(NodeId, Vec<(Option<String>, String)>)> = HashSet::new();
    if set_walk {
        seen.insert((anchor.ext.node, reg_key(&anchor.register)));
    }
    let mut advance = |frontier: &[GPath], round: usize| -> Vec<GPath> {
        let mut next = Vec::new();
        let current = promise.borrow().clone();
        for path in frontier {
            for alt in &group.alts {
                next.extend(expand_elems(
                    adapter,
                    alt,
                    vec![path.clone()],
                    trace,
                    outer,
                    witness,
                    budget.as_ref(),
                    current.as_deref(),
                ));
            }
        }
        let next = dedup_paths(next);
        // Under the acyclic promise no history can meet itself, so
        // the exact walk expands by node and round: the paths at one
        // position, one capsa and one arriving edge merge, their
        // grounds united (a return to any of them is a cycle).
        if alive.get() {
            let mut decl: Option<String> = None;
            let held = next.iter().all(|p| match &p.last_edge {
                Some(e) => match adapter.acyclic_link(&e.label) {
                    Some(d) => {
                        decl.get_or_insert(d);
                        true
                    }
                    None => false,
                },
                None => true,
            });
            if held {
                if decl.is_some() {
                    *promise.borrow_mut() = decl;
                }
            } else {
                alive.set(false);
                *promise.borrow_mut() = None;
            }
        }
        let next = if merge_ok && alive.get() && promise.borrow().is_some() {
            merge_by_position(next)
        } else {
            next
        };
        if !set_walk {
            record_debug(WALK, format!("round {round}"), || {
                format!(
                    "exact walk: {} expansion path{} in flight after the merge \
                     (frontier bound {f_max}, quantifier bound {n_max})",
                    next.len(),
                    if next.len() == 1 { "" } else { "s" }
                )
            });
            return next;
        }
        // The proximal reach selects one thread per node (spec:
        // Expansion paths): the first arrival of the round at a new
        // position stays, and the position is spent from then on.
        let mut fresh: Vec<GPath> = Vec::new();
        for p in next {
            let key = (p.ext.node, reg_key(&p.register));
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            fresh.push(p);
        }
        record_debug(WALK, format!("round {round}"), || {
            format!(
                "set walk: {} arrival{} this round, {} position{} spent",
                fresh.len(),
                if fresh.len() == 1 { "" } else { "s" },
                seen.len(),
                if seen.len() == 1 { "" } else { "s" }
            )
        });
        fresh
    };
    let mut matches: Vec<(GPath, usize)> = Vec::new();
    if group.quant.min == 0 && admits(&anchor) {
        matches.push((anchor.clone(), 0));
    }
    // The lightest walks under a budget (spec: The budget
    // quantifier, the proximal reach): breadth first by total —
    // each node settled once, at its least total, the first
    // satisfying settlement the answer for that node. Weights are
    // non-negative (a negative one refuses on the hop).
    if let (Some(b), Reach::Proximal) = (&budget, group.reach) {
        let mut settled: HashSet<NodeId> = HashSet::from([anchor.ext.node]);
        let mut pending: Vec<GPath> = Vec::new();
        for alt in &group.alts {
            pending.extend(expand_elems(
                adapter,
                alt,
                vec![anchor.clone()],
                trace,
                outer,
                witness,
                Some(b),
                promise.borrow().as_deref(),
            ));
        }
        let mut rounds = 0usize;
        while !pending.is_empty() {
            rounds += 1;
            if rounds > n_max * f_max {
                break;
            }
            // the least total first
            let (i, _) = pending
                .iter()
                .enumerate()
                .min_by(|(_, p), (_, q)| {
                    p.total
                        .partial_cmp(&q.total)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("non-empty");
            let path = pending.swap_remove(i);
            if !settled.insert(path.ext.node) {
                continue;
            }
            if admits(&path) {
                matches.push((path.clone(), 0));
            }
            if path.hops >= n_max {
                continue;
            }
            for alt in &group.alts {
                for p in expand_elems(
                    adapter,
                    alt,
                    vec![path.clone()],
                    trace,
                    outer,
                    witness,
                    Some(b),
                    promise.borrow().as_deref(),
                ) {
                    if !settled.contains(&p.ext.node) {
                        pending.push(p);
                    }
                }
            }
            if crate::value::ambient::refusal_pending() {
                return Vec::new();
            }
            if pending.len() > f_max {
                record_refusal_coded(
                    FRONTIER_BOUND,
                    format!("round {rounds}"),
                    format!(
                        "the frontier bound ({f_max}) is exceeded at round {rounds}: {} \
                         expansion paths in flight; give the quantifier a ceiling, \
                         {{;M}}, or raise --frontier-bound",
                        pending.len()
                    ),
                );
                return Vec::new();
            }
        }
        return exit_walk(
            matches.into_iter().map(|(p, _)| p).collect(),
            budget.as_ref(),
        );
    }
    let mut frontier = vec![anchor];
    let mut answered = false;
    for count in 1..=hi {
        frontier = advance(&frontier, count);
        // a refusal met on a hop (a cycle under the promise, a
        // negative weight) voids the walk, whatever it matched
        if crate::value::ambient::refusal_pending() {
            return Vec::new();
        }
        if frontier.is_empty() {
            break;
        }
        // The frontier bound: paths in flight after the round's
        // merge. A set walk never nears it (at most one arrival per
        // edge); an exact walk over data with many routes to a node
        // does, and refuses by name.
        if frontier.len() > f_max {
            record_refusal_coded(
                FRONTIER_BOUND,
                format!("round {count}"),
                format!(
                    "the frontier bound ({f_max}) is exceeded at round {count}: {} \
                     expansion paths in flight; give the quantifier a ceiling, \
                     {{;M}}, or raise --frontier-bound",
                    frontier.len()
                ),
            );
            return Vec::new();
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
    if !set_walk && group.quant.max.is_none() && !answered && !frontier.is_empty() {
        if !advance(&frontier, n_max + 1).is_empty() {
            record_refusal_coded(
                QUANTIFIER_BOUND,
                format!("round {n_max}"),
                format!(
                    "quantifier bound reached at round {n_max} with edges \
                     still to take — the closure would be silently \
                     incomplete; raise --quantifier-bound"
                ),
            );
            return Vec::new();
        }
    }
    let extreme = match group.reach {
        Reach::All => None,
        Reach::Proximal => matches.iter().map(|&(_, c)| c).min(),
        Reach::Distal => matches.iter().map(|&(_, c)| c).max(),
    };
    // Under a budget the distal reach is the heaviest walk within it,
    // the count of hops aside.
    let out: Vec<GPath> = if budget.is_some() && group.reach == Reach::Distal {
        let heaviest = matches
            .iter()
            .map(|(p, _)| p.total)
            .fold(f64::NEG_INFINITY, f64::max);
        matches
            .into_iter()
            .filter(|(p, _)| p.total == heaviest)
            .map(|(p, _)| p)
            .collect()
    } else {
        matches
            .into_iter()
            .filter(|&(_, c)| extreme.is_none_or(|e| c == e))
            .map(|(p, _)| p)
            .collect()
    };
    // The markers are the one per-node selection (spec: Expansion
    // paths): the proximal and the distal reach answer each node
    // once, its first walk at the tier; every other form every walk.
    let out: Vec<GPath> = if group.reach == Reach::All {
        out
    } else {
        let mut once: HashSet<(NodeId, Vec<(Option<String>, String)>)> = HashSet::new();
        out.into_iter()
            .filter(|p| once.insert((p.ext.node, reg_key(&p.register))))
            .collect()
    };
    let mut out = exit_walk(out, budget.as_ref());
    // An exact group inside a set walk: its results rejoin the set
    // walk with the anchor's ground, as set paths.
    if anchor_was_set && !set_walk {
        for p in &mut out {
            p.set_walk = true;
            p.visited = anchor_ground.clone();
        }
    }
    out
}

/// The walk's results leave the group with the walk's facts on them
/// (spec: Edge facts): the one command after the group reads `$-`
/// and `$-:round`, `$-:hops`, `$-:total` for the walk that ended
/// there.
fn exit_walk(mut out: Vec<GPath>, budget: Option<&BudgetBounds>) -> Vec<GPath> {
    for p in &mut out {
        p.ext.walk = Some(match budget {
            Some(b) => b.facts(p),
            None => p.facts(),
        });
    }
    out
}

/// Whether a quantifier's form asks for a node set alone: an open
/// quantifier with a floor of at most one, under all matches or the
/// proximal reach — the forms whose answer the distance expansion
/// gives exactly, since the nodes some acyclic walk reaches are the
/// nodes a shortest walk reaches (spec: Expansion paths). An exact
/// count, a ceiling, a floor above one or the distal reach ask about
/// acyclic walks of a given length and expand exactly.
pub(crate) fn set_form(group: &Group) -> bool {
    group.quant.min <= 1 && group.quant.max.is_none() && group.reach != Reach::Distal
}

/// Whether a pattern records nothing along the walk: no push and no
/// mark in any alternative, nested groups included. A recording walk
/// yields one result per walk, so it is exact whatever its form.
pub(crate) fn group_is_plain(group: &Group) -> bool {
    group.alts.iter().flatten().all(|e| match e {
        PathElem::Step(_) => true,
        PathElem::Group(g) => group_is_plain(g),
        PathElem::Mark(_) | PathElem::Push { .. } => false,
    })
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn group_admits(
    adapter: &impl AstAdapter,
    group: &Group,
    path: &GPath,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    budget: Option<&BudgetBounds>,
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
    // from it (`$$.x` inside the group's guard); the walk's facts
    // beside the edge.
    let facts = match budget {
        Some(b) => b.facts(path),
        None => path.facts(),
    };
    let scope = Scope {
        prov: None,
        outer,
        register: &path.register,
        crossing: path.last_edge.as_ref(),
        walk: Some(&facts),
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

/// Whether the group walks crosslinks alone: every hop of every
/// alternative, nested groups included, is a link hop by a label or
/// a wildcard — the walks the acyclic promise can spare their
/// histories. A child step or a navop star leaves a walk its
/// histories.
fn group_walks_links(group: &Group) -> bool {
    group.alts.iter().flatten().all(|elem| match elem {
        PathElem::Mark(_) | PathElem::Push { .. } => true,
        PathElem::Group(g) => group_walks_links(g),
        PathElem::Step(step) => {
            matches!(step.axis, Axis::OutLink | Axis::InLink | Axis::BothLink)
                && matches!(step.matcher, Matcher::Name(_) | Matcher::Any)
                && step.labels.is_empty()
        }
    })
}

/// The declaration promising every link the group walks acyclic
/// (spec: Model Files, Acyclic links) before the first round: each
/// hop a crosslink hop by an exact label the adapter promises. A
/// wildcard hop resolves it from the edges crossed instead.
fn group_promise(adapter: &impl AstAdapter, group: &Group) -> Option<String> {
    let mut decl: Option<String> = None;
    for alt in &group.alts {
        for elem in alt {
            match elem {
                PathElem::Mark(_) | PathElem::Push { .. } => {}
                PathElem::Group(g) => {
                    let d = group_promise(adapter, g)?;
                    decl.get_or_insert(d);
                }
                PathElem::Step(step) => {
                    if !matches!(step.axis, Axis::OutLink | Axis::InLink | Axis::BothLink) {
                        return None;
                    }
                    let Matcher::Name(label) = &step.matcher else {
                        return None;
                    };
                    let d = adapter.acyclic_link(label)?;
                    decl.get_or_insert(d);
                }
            }
        }
    }
    decl
}

/// A node named for a message by its path from the root
/// (`/stations/1`), the names the adapter gives its ancestors.
pub(crate) fn node_path(adapter: &impl AstAdapter, node: NodeId) -> String {
    let mut segments = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n == adapter.root() {
            break;
        }
        segments.push(adapter.name(n).unwrap_or_else(|| "*".to_string()));
        cur = adapter.parent(n);
    }
    segments.reverse();
    format!("/{}", segments.join("/"))
}

/// The exact walk under the acyclic promise: the paths of one round
/// standing at one node with one capsa and one arriving edge are one
/// expansion path, its ground the union of theirs — the nodes from
/// which this one was reached, any of which reached again is a
/// cycle. The breadcrumbs' provenance unites as in [`dedup_paths`].
fn merge_by_position(paths: Vec<GPath>) -> Vec<GPath> {
    let mut kept: Vec<GPath> = Vec::new();
    let mut index: HashMap<
        (
            NodeId,
            Vec<(Option<String>, String)>,
            Option<(NodeId, String, NodeId)>,
        ),
        usize,
    > = HashMap::new();
    for p in paths {
        let key = (
            p.ext.node,
            reg_key(&p.register),
            p.last_edge
                .as_ref()
                .map(|e| (e.source, e.label.clone(), e.target)),
        );
        match index.get(&key) {
            Some(&i) => {
                let k = &mut kept[i];
                for (a, b) in k.register.iter_mut().zip(&p.register) {
                    a.prov.origins.union(&b.prov.origins);
                }
                for n in p.visited {
                    if let Err(j) = k.visited.binary_search(&n) {
                        k.visited.insert(j, n);
                    }
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

/// Fold one alternative's elements over a set of in-flight paths.
/// Steps apply from each path's node and, on an exact walk's path, drop
/// successors the path has already visited; nested groups recurse
/// with the outer path's visited set threaded through (a simple path
/// never revisits a node, however deeply the pattern nests). A set
/// walk's path keeps no history: `expand_group` spends positions.
pub(crate) fn expand_elems(
    adapter: &impl AstAdapter,
    elems: &[PathElem],
    mut paths: Vec<GPath>,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    budget: Option<&BudgetBounds>,
    promise: Option<&str>,
) -> Vec<GPath> {
    for elem in elems {
        // A mark files a reference to each in-flight path's node.
        if let PathElem::Mark(name) = elem {
            for path in &mut paths {
                path.register
                    .push(mark_reg(adapter, path.ext.node, name.clone()));
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
                    // a link hop hands its edges over: one walk per
                    // edge (two rows on one leg, a node linked both
                    // ways)
                    let succs: Vec<(NodeId, Option<EdgeCtx>)> = if let Some(edges) = link_edges(
                        adapter,
                        path.ext.node,
                        step,
                        trace,
                        outer,
                        witness,
                        &path.ext,
                        &path.register,
                    ) {
                        edges.into_iter().map(|(s, e)| (s, Some(e))).collect()
                    } else {
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
                        succs
                            .into_iter()
                            .map(|s| (s, arrived_edge(adapter, path.ext.node, step, s)))
                            .collect()
                    };
                    for (s, edge) in succs {
                        // A simple path never revisits a node; under
                        // the acyclic promise a revisit is the data
                        // breaking it, refused naming the declaration
                        // (spec: Model Files, Acyclic links).
                        if path.blocks(s) {
                            if let Some(decl) = promise {
                                record_refusal_coded(
                                    crate::diagnostic::REFUSED,
                                    "the walk",
                                    format!(
                                        "a cycle under the promise '{decl}': the walk from \
                                         {} returns to {} after {} hop{}",
                                        node_path(adapter, path.visited[0]),
                                        node_path(adapter, s),
                                        path.hops + 1,
                                        if path.hops == 0 { "" } else { "s" }
                                    ),
                                );
                                return Vec::new();
                            }
                            continue;
                        }
                        let mut p = path.with(s, edge);
                        // Under a budget an edge is crossed with its
                        // weight, or not at all: a missing weight is no
                        // weight, never a free hop; past the ceiling the
                        // path ends here.
                        if let Some(b) = budget {
                            let Some(edge) = &p.last_edge else { continue };
                            match b.weight(adapter, edge) {
                                Ok(Some(w)) => {
                                    if w < 0.0 {
                                        record_refusal_coded(
                                            crate::diagnostic::REFUSED,
                                            "the quantifier",
                                            format!(
                                                "a negative weight ({w} on '{}') under a budget: the \
                                                 lightest walk needs weights of zero or more",
                                                edge.label
                                            ),
                                        );
                                        return Vec::new();
                                    }
                                    p.total += w;
                                    if b.ceiling.is_some_and(|c| p.total > c) {
                                        continue;
                                    }
                                }
                                Ok(None) => continue,
                                Err(e) => {
                                    record_refusal_coded(
                                        crate::diagnostic::REFUSED,
                                        "the quantifier",
                                        e,
                                    );
                                    return Vec::new();
                                }
                            }
                        }
                        next.push(p);
                    }
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
                    let facts = match budget {
                        Some(b) => b.facts(path),
                        None => path.facts(),
                    };
                    let scope = Scope {
                        prov: None,
                        outer,
                        crossing: path.last_edge.as_ref(),
                        walk: Some(&facts),
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
        // exact-walk paths at the same node with different histories
        // (ground covered, breadcrumbs, arrived-by edge) have
        // different futures. A set walk's path carries the anchor's
        // ground alone, so its key is position, capsa and edge.
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
                        row: None,
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
                        row: None,
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
                        row: None,
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
                        row: None,
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
                        row: None,
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
                        row: None,
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
                        row: None,
                    })
                },
            );
            out.extend(by_reach(found, *reach));
        }
        // the link axes: one successor per edge, the headless hop
        // both ways — `link_edges` is the arm, the edges it hands
        // over read by the walks that keep them
        Axis::OutLink | Axis::InLink => {
            out.extend(
                link_edges(adapter, node, step, trace, outer, witness, state, register)
                    .expect("a link axis")
                    .into_iter()
                    .map(|(n, _)| n),
            );
        }
        // `--` — both directions under one matcher: the node's
        // outgoing crosslinks and its backlinks, each edge kept
        // with its true direction so the edge context (`$-`) still
        // reports source and target faithfully.
        Axis::BothLink => both_link_step(
            adapter, node, step, trace, outer, witness, state, register, out,
        ),
        // `~` — every edge touching the node, the far ends the step
        // keeps; `~~` the walk over every edge ending at such a hop.
        Axis::Any => any_edge_step(
            adapter, node, step, trace, outer, witness, state, register, out,
        ),
        Axis::AnyDeep(reach) => any_deep_step(
            reach, adapter, node, step, trace, outer, witness, state, register, out,
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
                row: None,
            })
        },
    );
    out.extend(by_reach(found, *reach));
}

/// The edges touching `node` whose far end `step` keeps: the tree
/// edges by the far end's name (a child's, the parent's — the name
/// algebra and the nameless tests as on `/` and `\`), the crosslinks
/// by their label (as on `->` and `<-`, the nameless tests on the far
/// end). Each with its edge context, in the order children, parent,
/// outgoing, incoming. Siblings are an order, not edges.
pub(crate) fn incident_edges(
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
) -> Vec<(NodeId, EdgeCtx)> {
    let mut out = Vec::new();
    for c in adapter.children(node) {
        if matches_step(adapter, c, step) {
            out.push((
                c,
                EdgeCtx {
                    source: node,
                    label: CHILD.into(),
                    target: c,
                    row: None,
                },
            ));
        }
    }
    if let Some(p) = adapter.parent(node)
        && matches_step(adapter, p, step)
    {
        out.push((
            p,
            EdgeCtx {
                source: node,
                label: PARENT.into(),
                target: p,
                row: None,
            },
        ));
    }
    for (label, t, row) in select_links(step, &links_of(adapter, node), |l, _| {
        adapter.link_consumed(node, l)
    }) {
        if tests_ok(adapter, t, step) {
            out.push((
                t,
                EdgeCtx {
                    source: node,
                    label,
                    target: t,
                    row,
                },
            ));
        }
    }
    for (label, s, row) in select_links(step, &backlinks_of(adapter, node), |l, s| {
        adapter.link_consumed(s, l)
    }) {
        if tests_ok(adapter, s, step) {
            out.push((
                s,
                EdgeCtx {
                    source: s,
                    label,
                    target: node,
                    row,
                },
            ));
        }
    }
    out
}

/// The step `~*`: every edge, any far end.
fn wild_edge_step() -> Step {
    Step {
        axis: Axis::Any,
        matcher: Matcher::Any,
        labels: Vec::new(),
        traits: Vec::new(),
        predicates: Vec::new(),
        leaf: false,
    }
}

/// `~` — the navop star: every edge touching the node, either
/// direction, the far ends the step keeps. `apply_step`'s arm.
#[allow(clippy::too_many_arguments)]
pub(crate) fn any_edge_step(
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
    let matched = incident_edges(adapter, node, step);
    let kept = apply_predicates(
        adapter,
        matched,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |(n, _)| *n,
        |(_, e)| Some(e.clone()),
    );
    out.extend(kept.into_iter().map(|(n, _)| n));
}

/// `~~` — the deep navop star, `(~*)*~name`: the plain walk over
/// every edge from the node (each node reached once, at the fewest
/// rounds, the node itself at depth 0), then the matching hop from
/// every node reached, at the tier after its depth; the reach picks
/// the nearest or the farthest tier. `apply_step`'s arm.
#[allow(clippy::too_many_arguments)]
pub(crate) fn any_deep_step(
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
    let wild = wild_edge_step();
    let mut seen: HashSet<NodeId> = HashSet::from([node]);
    let mut reached: Vec<(NodeId, usize)> = vec![(node, 0)];
    let mut frontier = vec![node];
    let mut depth = 0;
    while !frontier.is_empty() {
        depth += 1;
        let mut next = Vec::new();
        for n in &frontier {
            for (far, _) in incident_edges(adapter, *n, &wild) {
                if seen.insert(far) {
                    next.push(far);
                }
            }
        }
        reached.extend(next.iter().map(|n| (*n, depth)));
        frontier = next;
    }
    let mut found: Vec<((NodeId, EdgeCtx), usize)> = Vec::new();
    for (n, d) in &reached {
        for hit in incident_edges(adapter, *n, step) {
            found.push((hit, d + 1));
        }
    }
    let kept = apply_predicates(
        adapter,
        found,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |((n, _), _)| *n,
        |((_, e), _)| Some(e.clone()),
    );
    out.extend(crate::graph::navigate::dedup(by_reach(
        kept.into_iter().map(|((n, _), d)| (n, d)).collect(),
        *reach,
    )));
}

/// `<->` — the links in both directions. `apply_step`'s arm.
#[allow(clippy::too_many_arguments)]
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
    out.extend(
        link_edges(adapter, node, step, trace, outer, witness, state, register)
            .expect("the headless axis")
            .into_iter()
            .map(|(n, _)| n),
    );
}

/// A link hop's successors, each with the edge it crosses: `->` the
/// outgoing links, `<-` the incoming, `--` both (the edge context
/// recording the edge's true direction — the walk just declined to
/// assert it). `None` for any other axis. One successor per EDGE:
/// a node reached by two rows of an edge-declared table, or linked
/// both ways, is reached twice, one thread per edge (spec: Expansion
/// paths, Edge rows), which is why the hop hands its edges over
/// rather than leaving them to be re-derived from the node
/// (`arrived_edge` finds the first).
#[allow(clippy::too_many_arguments)]
pub(crate) fn link_edges(
    adapter: &impl AstAdapter,
    node: NodeId,
    step: &Step,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    state: &GraphExt,
    register: &[Reg],
) -> Option<Vec<(NodeId, EdgeCtx)>> {
    let (outgoing, incoming) = match step.axis {
        Axis::OutLink => (true, false),
        Axis::InLink => (false, true),
        Axis::BothLink => (true, true),
        _ => return None,
    };
    let mut matched: Vec<(String, NodeId, bool, Option<NodeId>)> = Vec::new();
    if outgoing {
        matched.extend(
            select_links(step, &links_of(adapter, node), |l, _| {
                adapter.link_consumed(node, l)
            })
            .into_iter()
            .map(|(label, target, row)| (label, target, true, row)),
        );
    }
    if incoming {
        matched.extend(
            select_links(step, &backlinks_of(adapter, node), |l, s| {
                adapter.link_consumed(s, l)
            })
            .into_iter()
            .map(|(label, source, row)| (label, source, false, row)),
        );
    }
    matched.retain(|(_, other, _, _)| tests_ok(adapter, *other, step));
    let edge_of = |label: &String, other: NodeId, out: bool, row: Option<NodeId>| {
        if out {
            EdgeCtx {
                source: node,
                label: label.clone(),
                target: other,
                row,
            }
        } else {
            EdgeCtx {
                source: other,
                label: label.clone(),
                target: node,
                row,
            }
        }
    };
    let kept = apply_predicates(
        adapter,
        matched,
        step,
        trace,
        outer,
        witness,
        state,
        register,
        |(_, n, _, _)| *n,
        |(label, other, out, row)| Some(edge_of(label, *other, *out, *row)),
    );
    Some(
        kept.into_iter()
            .map(|(label, other, out, row)| (other, edge_of(&label, other, out, row)))
            .collect(),
    )
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
                        row: None,
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
        // A resolution edge is labeled by its relation — html's
        // rel — falling back to the property's reference label
        // (the name without its `_id` suffix).
        |&t| {
            Some(EdgeCtx {
                source: node,
                label: adapter
                    .ref_label(node, property)
                    .unwrap_or_else(|| reference_label(property).to_string()),
                target: t,
                row: None,
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
                row: None,
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
                label: adapter
                    .ref_label(source, p)
                    .unwrap_or_else(|| reference_label(p).to_string()),
                target: node,
                row: None,
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
        Axis::BothLink | Axis::Any | Axis::AnyDeep(_) => &[
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
    matches_name(adapter, node, &step.matcher)
        && algebra_ok(adapter, node, step)
        && tests_ok(adapter, node, step)
}

/// Whether `node` passes the name algebra of a step on a node axis
/// (`/(*.rs && !test_*)`, spec: Name algebra): every clause holds
/// of the node's name, the complement read among the hop's own
/// candidates, which is where the caller stands. A crosslink hop
/// reads its labels instead (`select_links`), and a step without
/// an algebra passes.
pub(crate) fn algebra_ok(adapter: &impl AstAdapter, node: NodeId, step: &Step) -> bool {
    step.axis.is_link()
        || step.labels.iter().all(|c| {
            c.alts
                .iter()
                .any(|a| matches_name(adapter, node, &a.matcher) != a.neg)
        })
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
            row: None,
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
                    walk: None,
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
                                walk: None,
                                witness: (!witness.is_empty()).then_some(witness),
                                ..Scope::at(state)
                            };
                            eval_pred_expr_at(
                                adapter,
                                node_of(item),
                                &truthy,
                                trace,
                                witness,
                                scope,
                            )
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
                        walk: None,
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

/// The crosslinks among `links` (label, other node, row) a step
/// keeps. By its matcher, one per edge; by its label selection
/// (`->(a && !b)`), one per other node, the selection reading the
/// labels the crosslinks to that node carry together (spec: Label
/// Selections) — the edge kept carries the first of them.
pub(crate) fn select_links(
    step: &Step,
    links: &[(String, NodeId, Option<NodeId>)],
    consumed: impl Fn(&str, NodeId) -> bool,
) -> Vec<(String, NodeId, Option<NodeId>)> {
    if step.labels.is_empty() {
        // A bare wildcard skips the references an edge declaration
        // consumed; a label named, a pattern or a selection reaches
        // them (spec: Edge rows). `consumed` is asked with the far
        // end, so a backward site can name the link's source.
        let wild = matches!(step.matcher, Matcher::Any);
        return links
            .iter()
            .filter(|(l, n, _)| matches_label(&step.matcher, l) && !(wild && consumed(l, *n)))
            .cloned()
            .collect();
    }
    // the name algebra reads the labels one edge carries: a row is
    // one edge of its own, a plain link's labels those to its far end
    let mut grouped: Vec<((NodeId, Option<NodeId>), Vec<String>)> = Vec::new();
    for (l, n, r) in links {
        match grouped.iter_mut().find(|(k, _)| *k == (*n, *r)) {
            Some((_, ls)) => ls.push(l.clone()),
            None => grouped.push(((*n, *r), vec![l.clone()])),
        }
    }
    grouped
        .into_iter()
        .filter(|(_, ls)| step.labels.iter().all(|c| c.matches(ls)))
        .map(|((n, r), mut ls)| (ls.swap_remove(0), n, r))
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
