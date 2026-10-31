//! The graph as the pipeline's host: every adapter is one. The
//! pipeline asks for what it needs of a node through a thread's
//! attachment and never names the node; this is where the question
//! meets the adapter and the graph's own evaluation.
use super::*;
use crate::value::Table;

impl<A: AstAdapter> Host for A {
    type Ext = GraphExt;
    type Trace = Correlation;

    fn root_ext(&self) -> GraphExt {
        GraphExt::at(self.root())
    }
    fn unplaced_ext(&self) -> GraphExt {
        GraphExt::unplaced(self.root())
    }
    fn placed(&self, ext: &GraphExt) -> bool {
        !ext.unplaced
    }
    fn scale_of(&self, expr: &str) -> Option<(f64, String)> {
        // The run's declared units first (the model's, installed for
        // the run by `with_unit_scale`), as every single-value
        // coercion reads them; the adapter's own otherwise — so a
        // reducer's stream reads `'18 parasang'` as `convert` does.
        crate::value::ambient::unit_scale(expr).unwrap_or_else(|| self.unit_scale(expr))
    }
    fn property_of(&self, ext: &GraphExt, name: &str) -> Option<Value> {
        self.property(ext.node, name)
    }
    fn properties_of(&self, ext: &GraphExt) -> Vec<(String, Value)> {
        self.properties(ext.node)
    }
    fn property_names_of(&self, ext: &GraphExt) -> Vec<String> {
        self.property_names(ext.node)
    }
    fn prefetch(&self, threads: &[Thread<GraphExt>], names: &[&str]) {
        let nodes: Vec<NodeId> = threads.iter().map(|c| c.ext.node).collect();
        self.prefetch_properties(&nodes, names);
    }
    fn prov_at(&self, ext: &GraphExt) -> Prov {
        if ext.unplaced {
            return Prov::at(Position::default(), command_now());
        }
        Prov::at(ext.node.into(), command_now())
    }
    fn origins_at(&self, ext: &GraphExt) -> Origins {
        if ext.unplaced {
            return Origins::at(Position::default(), command_now());
        }
        Origins::at(ext.node.into(), command_now())
    }
    fn origin_at(&self, ext: &GraphExt) -> Origin {
        Origin {
            position: if ext.unplaced {
                Position::default()
            } else {
                (ext.node).into()
            },
            command: command_now(),
        }
    }
    fn quantifier_bound(&self) -> usize {
        quantifier_bound(self)
    }
    fn node_ref(&self, ext: &GraphExt) -> Option<Value> {
        (!ext.unplaced).then(|| node_ref(self, ext.node))
    }
    fn witnessed(&self, ext: &GraphExt) -> bool {
        !ext.bindings.is_empty()
    }
    fn node_scalar(&self, ext: &GraphExt) -> Value {
        if ext.unplaced {
            return Value::Null;
        }
        node_scalar(self, ext.node)
    }
    fn project(&self, ext: &GraphExt, proj: &Projection) -> Value {
        if ext.unplaced {
            return Value::Null;
        }
        project(self, ext.node, proj)
    }
    fn origins_meta(&self, o: &Origins, key: &str) -> Option<Value> {
        origins_meta(self, o, key)
    }
    fn rel(
        &self,
        ext: &GraphExt,
        steps: &[PathElem],
        anchor: &Anchor,
        projection: Option<&Projection>,
        trace: &Correlation,
        scope: Scope<'_, GraphExt>,
    ) -> Vec<Value> {
        let nodes = rel_nodes(
            self,
            ext.node,
            steps,
            anchor,
            trace,
            witness_of(&scope),
            scope,
            false,
        );
        for &n in &nodes {
            note(n.into());
        }
        match projection {
            Some(p) => nodes.iter().map(|&n| project(self, n, p)).collect(),
            // The node side of the register read bare (`$$.name`,
            // `$$.N`, `((@))`) and an edge's end (`$-:source`):
            // references, as `$$_` is.
            None if steps.is_empty()
                && matches!(
                    anchor,
                    Anchor::Mark(_)
                        | Anchor::MarkIndex(_)
                        | Anchor::MarkTop
                        | Anchor::MarkBack(_)
                        | Anchor::MarksAll
                        | Anchor::MarksNamed(_)
                        | Anchor::EdgeEnd(_)
                ) =>
            {
                nodes.iter().map(|&n| node_ref(self, n)).collect()
            }
            None => vec![Value::Bool(true); nodes.len()],
        }
    }
    fn rel_exists(
        &self,
        ext: &GraphExt,
        steps: &[PathElem],
        anchor: &Anchor,
        projection: Option<&Projection>,
        trace: &Correlation,
        scope: Scope<'_, GraphExt>,
    ) -> bool {
        // nodes alone are read: a walk expands by node (spec:
        // Expansion paths)
        let nodes = rel_nodes(
            self,
            ext.node,
            steps,
            anchor,
            trace,
            witness_of(&scope),
            scope,
            true,
        );
        for &n in &nodes {
            note(n.into());
        }
        match projection {
            Some(p) => nodes.iter().any(|&n| project(self, n, p).is_truthy()),
            None => !nodes.is_empty(),
        }
    }
    fn rel_threads(
        &self,
        ext: &GraphExt,
        steps: &[PathElem],
        anchor: &Anchor,
        projection: Option<&Projection>,
        trace: &Correlation,
        scope: Scope<'_, GraphExt>,
    ) -> Vec<(GraphExt, Option<Value>, Prov)> {
        rel_nodes(
            self,
            ext.node,
            steps,
            anchor,
            trace,
            witness_of(&scope),
            scope,
            false,
        )
        .into_iter()
        .map(|n| {
            let topic = projection.map(|p| project(self, n, p));
            (
                self.restate(&scope, &GraphExt::at(n)),
                topic,
                Prov::at(n.into(), command_now()),
            )
        })
        .collect()
    }
    fn ctx(
        &self,
        ext: &GraphExt,
        index: Option<usize>,
        steps: &[PathElem],
        projection: Option<&Projection>,
        trace: &Correlation,
        scope: Scope<'_, GraphExt>,
    ) -> Vec<Value> {
        let bound = witness_of(&scope);
        let base = match index {
            Some(k) => bound.get(k.saturating_sub(1)).copied(),
            None => Some(Some(ext.node)),
        };
        let base = match base {
            None => return Vec::new(),
            Some(None) => return vec![Value::Null],
            Some(Some(n)) => n,
        };
        let nodes = if steps.is_empty() {
            vec![base]
        } else {
            navigate_from(
                steps,
                self,
                base,
                trace,
                nested_outer(&scope),
                bound,
                scope.register,
            )
        };
        for &n in &nodes {
            note(n.into());
        }
        match projection {
            Some(p) => nodes.iter().map(|&n| project(self, n, p)).collect(),
            // A bound node read bare (`$$1`) is a reference.
            None if steps.is_empty() && index.is_some() => {
                nodes.iter().map(|&n| node_ref(self, n)).collect()
            }
            None => vec![Value::Bool(true); nodes.len()],
        }
    }
    fn edges(&self, scope: Scope<'_, GraphExt>, projection: Option<&Projection>) -> Vec<Value> {
        let vs: Vec<Value> = scope
            .ext
            .arrived
            .iter()
            .map(|edge| match projection {
                None | Some(Projection::Property(None)) => Value::Str(edge.label.clone()),
                Some(Projection::Property(Some(prop))) => {
                    edge.property(self, prop).unwrap_or(Value::Null)
                }
                Some(_) => Value::Null,
            })
            .collect();
        vec![Value::list(vs)]
    }
    fn edge(&self, scope: Scope<'_, GraphExt>, projection: Option<&Projection>) -> Vec<Value> {
        let Some(edge) = scope.crossing else {
            return vec![Value::Null];
        };
        let v = match projection {
            None | Some(Projection::Property(None)) => Value::Str(edge.label.clone()),
            Some(Projection::Property(Some(prop))) => {
                edge.property(self, prop).unwrap_or(Value::Null)
            }
            // the edge facts (spec: Edge facts): what the walk knows
            // of the edge, null outside a quantified group
            Some(Projection::CoreMeta(key)) => {
                scope.walk.and_then(|w| w.fact(key)).unwrap_or(Value::Null)
            }
            Some(_) => Value::Null,
        };
        vec![v]
    }
    fn capsae(&self, scope: Scope<'_, GraphExt>, projection: Option<&Projection>) -> Vec<Value> {
        let Some(peers) = scope.peers else {
            return vec![Value::Null];
        };
        let vs: Vec<Value> = peers
            .caps
            .iter()
            .map(|p| match projection {
                None => {
                    match &p.topic {
                        Some(_) => note_all(&p.prov.origins),
                        None => note(p.ext.node.into()),
                    }
                    p.topic
                        .clone()
                        .unwrap_or_else(|| node_scalar(self, p.ext.node))
                }
                Some(proj) => {
                    note(p.ext.node.into());
                    project(self, p.ext.node, proj)
                }
            })
            .collect();
        vec![Value::list(vs)]
    }
    fn positions(&self, scope: Scope<'_, GraphExt>, projection: Option<&Projection>) -> Vec<Value> {
        match projection {
            Some(_) => self.capsae(scope, projection),
            None => {
                let Some(peers) = scope.peers else {
                    return vec![Value::Null];
                };
                let vs: Vec<Value> = peers
                    .caps
                    .iter()
                    .map(|p| {
                        note(p.ext.node.into());
                        node_ref(self, p.ext.node)
                    })
                    .collect();
                vec![Value::list(vs)]
            }
        }
    }
    fn operand_keys(
        &self,
        ext: &GraphExt,
        at: &Operand,
        trace: &Correlation,
        scope: Scope<'_, GraphExt>,
    ) -> Vec<NodeId> {
        operand_nodes(self, ext.node, at, trace, witness_of(&scope), scope)
    }
    fn restate(&self, scope: &Scope<'_, GraphExt>, at: &GraphExt) -> GraphExt {
        GraphExt {
            node: at.node,
            bindings: scope.ext.bindings.clone(),
            arrived: scope.ext.arrived.clone(),
            unplaced: at.unplaced,
            walk: None,
        }
    }
    fn exists_binding(
        &self,
        ext: &GraphExt,
        conds: &[&PredExpr],
        trace: &Correlation,
        scope: Scope<'_, GraphExt>,
    ) -> bool {
        exists_binding(self, ext.node, conds, trace, &mut Vec::new(), scope)
    }
    fn subcontext_traced(
        &self,
        body: &Query,
        ext: &GraphExt,
        trace: &Correlation,
        outer: Option<&Scope<'_, GraphExt>>,
    ) -> (Value, Prov) {
        subcontext_traced(body, self, ext.node, trace, outer)
    }
    fn navigate(
        &self,
        branch: &Branch,
        caps: Vec<Thread<GraphExt>>,
        trace: &Correlation,
        outer: Option<&Scope<'_, GraphExt>>,
        peers: Option<&Peers<GraphExt>>,
    ) -> Vec<Thread<GraphExt>> {
        let mut out = Vec::new();
        for c in &caps {
            // The thread's own pockets; an unset anchor yields
            // nothing, like every anchor miss. `(@)` re-seeds
            // the thread from every pocket — one fork per
            // marked node, registers and marks carried.
            let anchor_scope = c.scope(None, peers, outer);
            for from in anchor_nodes(self, &branch.anchor, c.ext.node, trace, &[], anchor_scope) {
                for (ext, register) in navigate_paths(
                    &branch.steps,
                    self,
                    from,
                    trace,
                    outer,
                    // The thread's witness travels with the hop, so
                    // step predicates on the way read `$*k` per
                    // thread (the results carry it on, below).
                    &c.ext.bindings,
                    &c.register,
                    false,
                ) {
                    out.push(Capsa {
                        prov: if branch.projection.is_some() {
                            Prov::at(ext.node.into(), command_now())
                        } else {
                            Prov::default()
                        },
                        register,
                        topic: branch
                            .projection
                            .as_ref()
                            .map(|p| project(self, ext.node, p)),
                        members: Vec::new(),
                        captures: c.captures.clone(),
                        named: c.named.clone(),
                        ext: GraphExt {
                            bindings: c.ext.bindings.clone(),
                            ..ext
                        },
                    });
                }
            }
        }
        // A hop folds the threads that arrive alike — one per (node,
        // arrival, register), the tree axes a node set (spec:
        // Expansion paths). A bare anchor is no hop: `| $$.H`,
        // `| $$.H::name` re-seat each thread on its marked node and
        // keep every thread, as the no-merge rule keeps every walk —
        // two threads at one node with one register are two threads
        // until `@| collapse`.
        if branch.steps.is_empty() && !matches!(branch.anchor, Anchor::Current) {
            return out;
        }
        dedup_merge(out)
    }
    fn node_to_json(&self, ext: &GraphExt) -> Value {
        node_to_json(self, ext.node)
    }
    fn node_to_json_text(&self, ext: &GraphExt) -> String {
        node_to_json_text(self, ext.node)
    }
    fn node_to_xml(&self, ext: &GraphExt) -> String {
        node_to_xml(self, ext.node)
    }
    fn node_table(&self, ext: &GraphExt) -> Result<Table, String> {
        crate::graph::tabula::node_table(self, ext.node)
    }
    fn render_node(&self, ext: &GraphExt, kind: &str) -> String {
        let kind = crate::graph::koine::Render::from_name(kind)
            .expect("command names are valid render names");
        crate::graph::koine::render_node(self, ext.node, kind)
    }
    fn value_to_xml(&self, v: &Value, tag: &str) -> String {
        value_to_xml(v, tag)
    }
    fn render_values(&self, values: &[Value], kind: &str) -> String {
        let kind = crate::graph::koine::Render::from_name(kind)
            .expect("command names are valid render names");
        crate::graph::koine::render_values(values, kind)
    }
    fn render_tree(
        &self,
        starts: &[GraphExt],
        spec: &crate::graph::tree::TreeSpec,
        column: &dyn Fn(&GraphExt, &Operand) -> Value,
    ) -> Result<String, String> {
        let nodes: Vec<NodeId> = starts.iter().map(|e| e.node).collect();
        Ok(crate::graph::tree::render(self, &nodes, spec, &|n, op| {
            column(&GraphExt::at(n), op)
        }))
    }
    fn graph_values(
        &self,
        starts: &[GraphExt],
        what: &crate::graph::edges::GraphTable,
        seeds: &[NodeId],
    ) -> Result<Vec<Value>, String> {
        let nodes: Vec<NodeId> = starts.iter().map(|e| e.node).collect();
        match what {
            crate::graph::edges::GraphTable::Pagerank {
                spec,
                damping,
                rounds,
            } => Ok(
                crate::graph::edges::pagerank_values(self, &nodes, spec, *damping, *rounds)?
                    .into_iter()
                    .map(Value::Float)
                    .collect(),
            ),
            crate::graph::edges::GraphTable::Distance { spec, .. } => {
                crate::graph::edges::distance_values(self, &nodes, spec, seeds)
            }
            crate::graph::edges::GraphTable::Kcore { spec } => {
                crate::graph::edges::kcore_values(self, &nodes, spec)
            }
            crate::graph::edges::GraphTable::Betweenness { spec, normalized } => {
                crate::graph::edges::betweenness_values(self, &nodes, spec, *normalized)
            }
            crate::graph::edges::GraphTable::Conncomp { spec } => {
                crate::graph::edges::conncomp_values(self, &nodes, spec)
            }
            _ => Err("the graph library answers a value per node".into()),
        }
    }
    fn graph_table(
        &self,
        starts: &[GraphExt],
        what: &crate::graph::edges::GraphTable,
    ) -> Result<Table, String> {
        // the context's nodes, each once in order of first arrival:
        // several threads may stand on one (spec: Expansion paths)
        let mut nodes: Vec<NodeId> = Vec::new();
        for e in starts {
            if !nodes.contains(&e.node) {
                nodes.push(e.node);
            }
        }
        match what {
            crate::graph::edges::GraphTable::Edges(spec) => {
                crate::graph::edges::edges_table(self, &nodes, spec)
            }
            crate::graph::edges::GraphTable::Nodes => {
                Ok(crate::graph::edges::nodes_table(self, &nodes))
            }
            crate::graph::edges::GraphTable::Pagerank { .. }
            | crate::graph::edges::GraphTable::Distance { .. }
            | crate::graph::edges::GraphTable::Kcore { .. }
            | crate::graph::edges::GraphTable::Betweenness { .. }
            | crate::graph::edges::GraphTable::Conncomp { .. } => {
                Err("the graph library answers a value per node, not a table".into())
            }
        }
    }
}

/// Turn the final capsa context into a query result. `projected` is
/// the query's static value-vs-node typing (see [`eval_query`]); it
/// decides the empty case, where no capsa is left to inspect.
pub(crate) fn to_result(caps: Vec<Capsa>, projected: bool) -> QueryResult {
    if projected || caps.iter().any(|c| c.topic.is_some()) {
        QueryResult::Values(
            caps.into_iter()
                .map(|c| c.topic.unwrap_or(Value::Null))
                .collect(),
        )
    } else {
        QueryResult::Nodes(caps.into_iter().map(|c| c.ext.node).collect())
    }
}

/// A final capsa with its provenance: the node it stands at, its
/// topic (`None` for a node result), and where the topic's value
/// — per field, for a record — was read.
#[derive(Clone, Debug)]
pub struct Traced {
    pub node: NodeId,
    pub topic: Option<Value>,
    pub prov: Prov,
}

/// A node's provenance as the query answers it: the adapter's
/// entries (one empty entry where it records nothing), each with
/// its path filled from the arbor root where no layer set it, and
/// the instant ladder completed — the datum's own timestamp, else
/// the source's modification time (both the adapters' work), else
/// the moment of reading, the invocation instant. Under a
/// reproducible run ([`RunOptions::reproducible`]) the last two
/// rungs are held back: only what the data itself recorded travels.
pub fn resolved_provenance<A: AstAdapter + ?Sized>(adapter: &A, node: NodeId) -> ProvenanceList {
    let mut list = adapter.provenance_list(node);
    if list.entries.is_empty() {
        list.entries.push(Provenance::default());
    }
    let (reproducible, access) = run_option(|o| (o.reproducible, o.now));
    let mut path: Option<String> = None;
    for e in &mut list.entries {
        if e.path.is_none() {
            e.path = Some(path.get_or_insert_with(|| name_path(adapter, node)).clone());
        }
        if reproducible {
            if matches!(
                e.instant_from,
                Some(InstantFrom::Source | InstantFrom::Access)
            ) {
                e.instant = None;
                e.instant_from = None;
            }
        } else if e.instant.is_none()
            && let Some((secs, nanos)) = access
        {
            e.instant = Some((secs, nanos, Some(0)));
            e.instant_from = Some(InstantFrom::Access);
        }
    }
    list
}

/// A value's provenance keys over its origin set: `source` the
/// anchoring (first) origin's, `dpid` likewise, `instant` the latest
/// (a derived value is as fresh as its newest input), `provenance`
/// the canonical list `?a@ts#x;b#y` (entries deduplicated by
/// source and dpid, first-read order), `origins` one canonical text
/// per origin, `elided` how many the bound dropped.
pub(crate) fn origins_meta(adapter: &impl AstAdapter, o: &Origins, key: &str) -> Option<Value> {
    let (entries, elided) = origin_entries(adapter, o);
    match key {
        "source" => Some(
            entries
                .first()
                .and_then(|p| p.source.clone())
                .map_or(Value::Null, Value::Str),
        ),
        "dpid" => Some(
            entries
                .first()
                .and_then(|p| p.dpid.clone())
                .map_or(Value::Null, Value::Str),
        ),
        "path" => Some(
            entries
                .first()
                .and_then(|p| p.path.clone())
                .map_or(Value::Null, Value::Str),
        ),
        "instant" => Some(
            entries
                .iter()
                .filter_map(|p| p.instant)
                .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)))
                .map_or(Value::Null, |(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                    calendar: Default::default(),
                }),
        ),
        "provenance" => {
            Some(Provenance::canonical_list(&entries, elided).map_or(Value::Null, Value::Str))
        }
        // The record: the same answers the singular keys give.
        "%provenance" => {
            let newest = entries
                .iter()
                .filter(|p| p.instant.is_some())
                .max_by(|a, b| {
                    (a.instant.unwrap().0, a.instant.unwrap().1)
                        .cmp(&(b.instant.unwrap().0, b.instant.unwrap().1))
                });
            // … plus the elided count and the canonical text, so
            // the record is the whole of the singular keys (spec:
            // Record Fields, draft 157).
            let Value::Record(mut fields) = prov_record(&Provenance {
                source: entries.first().and_then(|p| p.source.clone()),
                path: entries.first().and_then(|p| p.path.clone()),
                instant: newest.and_then(|p| p.instant),
                instant_from: newest.and_then(|p| p.instant_from),
                dpid: entries.first().and_then(|p| p.dpid.clone()),
            }) else {
                unreachable!("prov_record builds a record")
            };
            fields.push(("elided".to_string(), Value::Int(elided as i64)));
            fields.push((
                "text".to_string(),
                Provenance::canonical_list(&entries, elided).map_or(Value::Null, Value::Str),
            ));
            Some(Value::Record(fields))
        }
        "@provenance" => Some(Value::list(entries.iter().map(prov_record).collect())),
        // The coordinates: the first origin's id, its record, all.
        "origin" => Some(
            o.known
                .first()
                .map_or(Value::Null, |x| Value::Int(x.position.0 as i64)),
        ),
        "%origin" => Some(o.known.first().map_or(Value::Null, |x| origin_record(*x))),
        "@origin" => Some(Value::list(
            o.known.iter().map(|x| origin_record(*x)).collect(),
        )),
        "elided" => Some(Value::Int(elided as i64)),
        _ => None,
    }
}

/// The origins' provenance entries, resolved through the adapter's
/// layers (an origin that itself records a list contributes every
/// entry), deduplicated by (source, dpid) in first-read order, with
/// the count elided: by the origin bound, and by the origins' own
/// records. An origin that answers nothing contributes nothing.
pub(crate) fn origin_entries(adapter: &impl AstAdapter, o: &Origins) -> (Vec<Provenance>, u32) {
    let mut out: Vec<Provenance> = Vec::new();
    let mut elided = o.more;
    for n in o.positions().map(NodeId::from) {
        let list = resolved_provenance(adapter, n);
        elided = elided.saturating_add(list.elided);
        for p in list.entries {
            // One entry per data point: the source's own key where
            // it assigned one (several nodes under one dpid are one
            // datum by the source's claim), the path otherwise.
            let key =
                |e: &Provenance| (e.source.clone(), e.dpid.clone().or_else(|| e.path.clone()));
            let pk = key(&p);
            match out.iter_mut().find(|q| key(q) == pk) {
                Some(q) => {
                    // The same datum read twice: the latest observation.
                    if p.instant.map(|i| (i.0, i.1)) > q.instant.map(|i| (i.0, i.1)) {
                        q.instant = p.instant;
                        q.instant_from = p.instant_from;
                    }
                }
                None => out.push(p),
            }
        }
    }
    (out, elided)
}

/// Evaluate `query` from the root.
/// Refuse a query that uses the `sh(...)` command unless the adapter
/// allows it (`qua --allow-shell`). Query text stays inert data by
/// default — a `.quarb` file, a defs file, or a macro can never run
/// a command without the explicit per-run opt-in.
/// The quantifier bound in force: the run's, else the adapter's.
pub(crate) fn quantifier_bound(adapter: &impl AstAdapter) -> usize {
    run_option(|o| o.quantifier_bound).unwrap_or_else(|| adapter.quantifier_bound())
}

/// The frontier bound in force: the run's, else the adapter's.
pub(crate) fn frontier_bound(adapter: &impl AstAdapter) -> usize {
    run_option(|o| o.frontier_bound).unwrap_or_else(|| adapter.frontier_bound())
}

pub fn eval(query: &Query, adapter: &impl AstAdapter) -> QueryResult {
    eval_with(query, adapter, None)
}

/// [`eval`] under `options` (`None`: the enclosing run's, or the
/// defaults at the top).
pub(crate) fn eval_with(
    query: &Query,
    adapter: &impl AstAdapter,
    options: Option<Run>,
) -> QueryResult {
    let _run = enter(options);
    // the mounted document's declared units reach every quantity
    // coercion of the run, the table builder's included
    crate::value::ambient::with_unit_scale(&|e| adapter.unit_scale(e), || {
        eval_query(query, adapter, adapter.root(), &Correlation::default())
    })
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
    options: Option<Run>,
) -> Vec<Traced> {
    let _run = enter(options);
    let (caps, projected) =
        crate::value::ambient::with_unit_scale(&|e| adapter.unit_scale(e), || {
            eval_query_caps(query, adapter, adapter.root(), &Correlation::default())
        });
    caps.into_iter()
        .map(|c| {
            let topic = if projected {
                Some(c.topic.unwrap_or(Value::Null))
            } else {
                c.topic
            };
            Traced {
                node: c.ext.node,
                topic,
                prov: c.prov,
            }
        })
        .collect()
}

/// The result of evaluating a query: a node set, or\,---\,after a
/// projection\,---\,a list of scalar values (one per node).
#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    /// Nodes selected by navigation (no projection).
    Nodes(Vec<NodeId>),
    /// Scalars produced by a projection.
    Values(Vec<Value>),
}

/// The calculator's host: no arbor. Every thread stands nowhere, a
/// value is all it carries, and what names a node is refused by
/// name, as the command line refuses it before the run.
pub struct Calc;

fn no_arbor(what: &str) -> String {
    format!("qc is the calculator and has no source: {what} names a node")
}

/// Why the calculator cannot run a query: a head that is not bare
/// (an anchor, a step, a correlation), or a command of the graph's
/// layer in its pipeline (the registry says which).
fn calc_refuses(query: &Query) -> Option<String> {
    if !is_bare(query) {
        return Some(no_arbor("the query's head"));
    }
    query.pipeline.iter().find_map(|c| match c {
        Command::Func(call) | Command::Agg(call)
            if crate::syntax::registry::layer(&call.name)
                == Some(crate::syntax::registry::Layer::Node) =>
        {
            Some(no_arbor(&format!("the command `{}`", call.name)))
        }
        _ => None,
    })
}

impl Host for Calc {
    type Ext = ();
    type Trace = ();

    fn root_ext(&self) {}
    fn unplaced_ext(&self) {}
    fn placed(&self, _: &()) -> bool {
        false
    }
    fn scale_of(&self, expr: &str) -> Option<(f64, String)> {
        crate::value::quantity::scale_expr(expr)
    }
    fn property_of(&self, _: &(), _: &str) -> Option<Value> {
        None
    }
    fn property_names_of(&self, _: &()) -> Vec<String> {
        Vec::new()
    }
    fn properties_of(&self, _: &()) -> Vec<(String, Value)> {
        Vec::new()
    }
    fn prefetch(&self, _: &[Thread<()>], _: &[&str]) {}
    fn prov_at(&self, _: &()) -> Prov {
        Prov::at(Position::default(), command_now())
    }
    fn origin_at(&self, _: &()) -> Origin {
        Origin {
            command: command_now(),
            ..Origin::default()
        }
    }
    fn origins_at(&self, _: &()) -> Origins {
        Origins::at(Position::default(), command_now())
    }
    fn quantifier_bound(&self) -> usize {
        // the run's bound first (`--quantifier-bound`), as the graph
        // host takes it; the bare adapter's only without one
        quantifier_bound(&crate::graph::adapter::Bare)
    }
    fn node_ref(&self, _: &()) -> Option<Value> {
        record_refusal(no_arbor("a mark"));
        None
    }
    fn witnessed(&self, _: &()) -> bool {
        false
    }
    fn node_scalar(&self, _: &()) -> Value {
        Value::Null
    }
    fn project(&self, _: &(), _: &Projection) -> Value {
        Value::Null
    }
    fn origins_meta(&self, o: &Origins, key: &str) -> Option<Value> {
        origins_meta(&crate::graph::adapter::Bare, o, key)
    }
    fn rel(
        &self,
        _: &(),
        _: &[PathElem],
        _: &Anchor,
        _: Option<&Projection>,
        _: &(),
        _: Scope<'_, ()>,
    ) -> Vec<Value> {
        record_refusal(no_arbor("a path"));
        Vec::new()
    }
    fn rel_threads(
        &self,
        _: &(),
        _: &[PathElem],
        _: &Anchor,
        _: Option<&Projection>,
        _: &(),
        _: Scope<'_, ()>,
    ) -> Vec<((), Option<Value>, Prov)> {
        record_refusal(no_arbor("a path"));
        Vec::new()
    }
    fn ctx(
        &self,
        _: &(),
        _: Option<usize>,
        _: &[PathElem],
        _: Option<&Projection>,
        _: &(),
        _: Scope<'_, ()>,
    ) -> Vec<Value> {
        record_refusal(no_arbor("a context reach"));
        Vec::new()
    }
    fn edges(&self, _: Scope<'_, ()>, _: Option<&Projection>) -> Vec<Value> {
        vec![Value::list(Vec::new())]
    }
    fn edge(&self, _: Scope<'_, ()>, _: Option<&Projection>) -> Vec<Value> {
        vec![Value::Null]
    }
    fn capsae(&self, scope: Scope<'_, ()>, _: Option<&Projection>) -> Vec<Value> {
        let Some(peers) = scope.peers else {
            return vec![Value::Null];
        };
        let vs: Vec<Value> = peers
            .caps
            .iter()
            .map(|p| {
                if let Some(_) = &p.topic {
                    note_all(&p.prov.origins);
                }
                p.topic.clone().unwrap_or(Value::Null)
            })
            .collect();
        vec![Value::list(vs)]
    }
    fn positions(&self, _: Scope<'_, ()>, _: Option<&Projection>) -> Vec<Value> {
        record_refusal(no_arbor("the positions '@@_'"));
        vec![Value::Null]
    }
    fn operand_keys(&self, _: &(), _: &Operand, _: &(), _: Scope<'_, ()>) -> Vec<()> {
        record_refusal(no_arbor("a peer read"));
        Vec::new()
    }
    fn restate(&self, _: &Scope<'_, ()>, _: &()) {}
    fn exists_binding(
        &self,
        ext: &(),
        conds: &[&PredExpr],
        trace: &(),
        scope: Scope<'_, ()>,
    ) -> bool {
        // No witness to search: the conditions hold or do not.
        conds
            .iter()
            .all(|e| eval_pred_expr(self, ext, e, trace, scope))
    }
    fn subcontext_traced(
        &self,
        body: &Query,
        _: &(),
        _: &(),
        outer: Option<&Scope<'_, ()>>,
    ) -> (Value, Prov) {
        let (caps, projected) = eval_bare(body, self, outer);
        if projected {
            let mut vs: Vec<(Value, Prov)> = caps
                .into_iter()
                .map(|c| (c.topic.clone().unwrap_or(Value::Null), c.prov))
                .collect();
            match vs.len() {
                0 => (Value::Null, Prov::default()),
                1 => vs.pop().unwrap(),
                _ => {
                    let (values, provs): (Vec<Value>, Vec<Prov>) = vs.into_iter().unzip();
                    (Value::list(values), Prov::list(provs))
                }
            }
        } else {
            (Value::Int(caps.len() as i64), Prov::default())
        }
    }
    fn navigate(
        &self,
        _: &Branch,
        _: Vec<Thread<()>>,
        _: &(),
        _: Option<&Scope<'_, ()>>,
        _: Option<&Peers<()>>,
    ) -> Vec<Thread<()>> {
        record_refusal(no_arbor("a navigation command"));
        Vec::new()
    }
    fn node_to_json(&self, _: &()) -> Value {
        Value::Null
    }
    fn node_to_json_text(&self, _: &()) -> String {
        "null".to_string()
    }
    fn node_to_xml(&self, _: &()) -> String {
        String::new()
    }
    fn node_table(&self, _: &()) -> Result<Table, String> {
        Err(no_arbor("a table of a node"))
    }
    fn render_node(&self, _: &(), _: &str) -> String {
        String::new()
    }
    fn value_to_xml(&self, v: &Value, tag: &str) -> String {
        value_to_xml(v, tag)
    }
    fn render_values(&self, values: &[Value], kind: &str) -> String {
        let kind = crate::graph::koine::Render::from_name(kind)
            .expect("command names are valid render names");
        crate::graph::koine::render_values(values, kind)
    }
    fn render_tree(
        &self,
        _: &[()],
        _: &crate::graph::tree::TreeSpec,
        _: &dyn Fn(&(), &Operand) -> Value,
    ) -> Result<String, String> {
        Err(no_arbor("the command `tree`"))
    }
    fn graph_table(
        &self,
        _: &[()],
        what: &crate::graph::edges::GraphTable,
    ) -> Result<Table, String> {
        Err(no_arbor(match what {
            crate::graph::edges::GraphTable::Edges(_) => "the command `edges`",
            crate::graph::edges::GraphTable::Nodes => "the command `nodes`",
            crate::graph::edges::GraphTable::Pagerank { .. }
            | crate::graph::edges::GraphTable::Distance { .. }
            | crate::graph::edges::GraphTable::Kcore { .. }
            | crate::graph::edges::GraphTable::Betweenness { .. }
            | crate::graph::edges::GraphTable::Conncomp { .. } => "the graph library",
        }))
    }
    fn graph_values(
        &self,
        _: &[()],
        _: &crate::graph::edges::GraphTable,
        _: &[()],
    ) -> Result<Vec<Value>, String> {
        Err(no_arbor("the graph library"))
    }
}

/// Evaluate a query at the calculator: the values it ends with. A
/// query that is not bare (an anchor, a step, a correlation) runs as
/// the pipeline over nothing and is refused where it names a node.
pub fn eval_calc(query: &Query, options: Option<Run>) -> Vec<Value> {
    let _run = enter(options);
    if let Some(why) = calc_refuses(query) {
        record_refusal(why);
        return Vec::new();
    }
    let (caps, _) = eval_bare(query, &Calc, None);
    caps.into_iter()
        .map(|c| c.topic.unwrap_or(Value::Null))
        .collect()
}

/// The traced form: every value with its provenance, for the kaiv
/// output and the saved results.
pub fn eval_calc_traced(query: &Query, options: Option<Run>) -> Vec<Traced> {
    let _run = enter(options);
    if let Some(why) = calc_refuses(query) {
        record_refusal(why);
        return Vec::new();
    }
    let (caps, projected) = eval_bare(query, &Calc, None);
    caps.into_iter()
        .map(|c| {
            let topic = if projected {
                Some(c.topic.unwrap_or(Value::Null))
            } else {
                c.topic
            };
            Traced {
                node: Origin::default().position.into(),
                topic,
                prov: c.prov,
            }
        })
        .collect()
}

/// A provenance entry as a record: `%(source; path; instant;
/// instant-from; dpid)`, absent components null.
pub(crate) fn prov_record(p: &Provenance) -> Value {
    Value::record(vec![
        (
            "source".to_string(),
            p.source.clone().map_or(Value::Null, Value::Str),
        ),
        (
            "path".to_string(),
            p.path.clone().map_or(Value::Null, Value::Str),
        ),
        (
            "instant".to_string(),
            p.instant
                .map_or(Value::Null, |(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                    calendar: Default::default(),
                }),
        ),
        (
            "instant-from".to_string(),
            p.instant_from
                .map_or(Value::Null, |f| Value::Str(f.name().to_string())),
        ),
        (
            "dpid".to_string(),
            p.dpid.clone().map_or(Value::Null, Value::Str),
        ),
    ])
}

/// An origin coordinate as a record: `%(id; command)`.
pub(crate) fn origin_record(o: Origin) -> Value {
    Value::record(vec![
        ("id".to_string(), Value::Int(o.position.0 as i64)),
        ("command".to_string(), Value::Int(o.command as i64)),
    ])
}
