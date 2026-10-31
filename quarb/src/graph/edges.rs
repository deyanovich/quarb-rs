//! `@| edges` and `@| nodes`: the graph as a table (spec: Aggregate
//! Commands, "The graph as a table", draft 181). `edges` answers the
//! induced subgraph of its context — one row per stored edge whose
//! two ends are both in the context — with the columns `source`,
//! `target` (node references, in the declaration's orientation),
//! `label`, `directed` and the edge's own properties; `nodes` the
//! context's nodes as `node` (a reference), `name`, `kind`. A node
//! reference prints as the node's path (`/stations/2`) and stands as
//! a node operand inside the query (`:source:.l`, the regula
//! attached to the edge's source).

use super::adapter::AstAdapter;
use super::navigate::EdgeCtx;
use super::navigate::{CHILD, core_meta, node_ref};
use super::steps::BudgetBounds;
use crate::NodeId;
use crate::syntax::ast::{
    Anchor, Arg, Axis, FnCall, LabelClause, Matcher, Operand, PathElem, Projection,
};
use crate::value::{Table, Value};
use std::collections::{HashMap, HashSet};

/// Which edges `edges` collects, as the hop its operand spells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collect {
    /// `->label`: the outgoing stored edges, directed.
    Out,
    /// `<-label`: the incoming stored edges, directed.
    In,
    /// `--label`: the stored edges either way, `directed` false.
    Both,
    /// `~label`: every edge the navop star walks, tree edges
    /// included, `directed` false.
    Any,
}

/// One collecting hop of an `edges` / `pagerank` call: the axis,
/// the label it names (`None` for a wildcard) and the name algebra
/// on it (`->(a || !b)`).
#[derive(Clone, Debug)]
pub struct Hop {
    pub collect: Collect,
    pub label: Option<String>,
    pub labels: Vec<LabelClause>,
}

impl Hop {
    /// A bare wildcard (`->*`, `--*`, `~*`): the hop that skips the
    /// references an edge declaration consumed (spec: Edge rows).
    fn wild(&self) -> bool {
        self.label.is_none() && self.labels.is_empty()
    }

    fn admits(&self, label: &str) -> bool {
        self.label.as_deref().is_none_or(|l| l == label)
            && self
                .labels
                .iter()
                .all(|c| c.matches(std::slice::from_ref(&label.to_string())))
    }
}

/// The `edges` call read: the hops (one, or the alternatives of a
/// group, `(->road|->rail)`) and the `--weight` option.
#[derive(Clone, Debug)]
pub struct EdgesSpec {
    pub hops: Vec<Hop>,
    pub weight: Option<Weight>,
}

/// `--weight=…` (spec: The graph as a table), two forms: a UNIT
/// selects the edge property of that unit's dimension and converts
/// it, as a budget does; `::prop` names the property and takes its
/// values as they stand — the way a dimensionless weight is chosen
/// (a unit never starts with the sigil, a property always does).
#[derive(Clone, Debug, PartialEq)]
pub enum Weight {
    Unit(String),
    Property(String),
}

/// What the graph-table command draws, or the graph library ranks.
#[derive(Clone, Debug)]
pub enum GraphTable {
    Edges(EdgesSpec),
    Nodes,
    /// `@| graph:pagerank(->link; --damping=d --rounds=n
    /// --weight=…)` (spec: The graph library) — the rank over the
    /// induced subgraph by the hop's edges, a float per node (the
    /// bare `@| pagerank` table of 0.44 is retired).
    Pagerank {
        spec: EdgesSpec,
        damping: f64,
        rounds: usize,
    },
    /// `@| graph:distance(->*; --from=NODES; --weight=…)`: each
    /// node's shortest distance from the seed set along the hop —
    /// hops, or the weights summed.
    Distance {
        spec: EdgesSpec,
        from: Operand,
    },
    /// `@| graph:kcore(--*)`: each node's core number over the
    /// undirected distinct neighbours.
    Kcore {
        spec: EdgesSpec,
    },
    /// `@| graph:betweenness(->*; --normalized --weight=…)`: each
    /// node's betweenness centrality, Brandes.
    Betweenness {
        spec: EdgesSpec,
        normalized: bool,
    },
    /// `@| graph:conncomp(--*)`: each node's connected component,
    /// numbered from 1 by the components' first nodes.
    Conncomp {
        spec: EdgesSpec,
    },
}

fn hop_of(step: &crate::syntax::ast::Step) -> Result<Hop, String> {
    if !step.predicates.is_empty() || !step.traits.is_empty() {
        return Err(hop_message());
    }
    let collect = match step.axis {
        Axis::OutLink => Collect::Out,
        Axis::InLink => Collect::In,
        Axis::BothLink => Collect::Both,
        Axis::Any => Collect::Any,
        _ => return Err(hop_message()),
    };
    let label = match &step.matcher {
        Matcher::Name(l) => Some(l.clone()),
        Matcher::Any => None,
        _ => return Err(hop_message()),
    };
    Ok(Hop {
        collect,
        label,
        labels: step.labels.clone(),
    })
}

impl GraphTable {
    pub(crate) fn from_call(call: &FnCall) -> Result<GraphTable, String> {
        if call.name == "nodes" {
            if !call.args.is_empty() {
                return Err("nodes takes no operand: the context is the table".into());
            }
            return Ok(GraphTable::Nodes);
        }
        let name = call.name.as_str();
        // the graph library's commands (spec: The graph library), or
        // the edges table
        let library = name.starts_with("graph:");
        let ranks = name == "graph:pagerank";
        let distance = name == "graph:distance";
        let kcore = name == "graph:kcore";
        let betweenness = name == "graph:betweenness";
        let conncomp = name == "graph:conncomp";
        let shown = name;
        let mut from: Option<Operand> = None;
        let mut normalized = false;
        let mut spec = EdgesSpec {
            hops: Vec::new(),
            weight: None,
        };
        let mut damping = quarb_lib_graph::DAMPING;
        let mut rounds = quarb_lib_graph::ROUNDS;
        let number = |v: &Value| -> Option<f64> {
            match v {
                Value::Float(f) => Some(*f),
                Value::Int(i) => Some(*i as f64),
                _ => None,
            }
        };
        for arg in &call.args {
            match arg {
                // `--weight=UNIT`: the unit's text
                // `--from=NODES`: the seeds of a distance, an
                // anchored node operand resolved at the run
                Arg::Opt {
                    name: opt,
                    value: Some(op),
                } if distance && (opt == "--from" || opt == "-f") => {
                    if !matches!(
                        op,
                        Operand::Rel {
                            projection: None,
                            ..
                        }
                    ) {
                        return Err(
                            "graph:distance: --from takes a node operand (a path from the root, a mark $$.S, an id anchor ((!7)))"
                                .into(),
                        );
                    }
                    from = Some(op.clone());
                }
                Arg::Opt { name: opt, .. } if kcore && (opt == "--weight" || opt == "-w") => {
                    return Err("graph:kcore counts distinct neighbours and takes no weight".into());
                }
                Arg::Opt { name: opt, .. } if conncomp && (opt == "--weight" || opt == "-w") => {
                    return Err(
                        "graph:conncomp follows the edges as they stand and takes no weight".into(),
                    );
                }
                Arg::Opt {
                    name: opt,
                    value: None,
                } if betweenness && (opt == "--normalized" || opt == "-N") => {
                    normalized = true;
                }
                Arg::Opt {
                    name: opt,
                    value: Some(Operand::Lit(v)),
                } if opt == "--weight" || opt == "-w" => {
                    spec.weight = Some(Weight::Unit(v.to_string()));
                }
                // `--weight=::prop`: the property by name
                Arg::Opt {
                    name: opt,
                    value:
                        Some(Operand::Rel {
                            steps,
                            projection: Some(Projection::Property(Some(prop))),
                            anchor: Anchor::Current,
                        }),
                } if (opt == "--weight" || opt == "-w") && steps.is_empty() => {
                    spec.weight = Some(Weight::Property(prop.clone()));
                }
                Arg::Opt { name: opt, .. } if opt == "--weight" || opt == "-w" => {
                    return Err(format!(
                        "{shown}: --weight takes a unit (--weight=km) or a property (--weight=::weight)"
                    ));
                }
                Arg::Opt {
                    name: opt,
                    value: Some(Operand::Lit(v)),
                } if ranks && (opt == "--damping" || opt == "-d") => {
                    damping = number(v).ok_or_else(|| {
                        format!("{shown}: --damping takes a number, '--damping=0.85'")
                    })?;
                    if !(0.0..=1.0).contains(&damping) {
                        return Err(format!("{shown}: the damping {damping} is outside 0..1"));
                    }
                }
                Arg::Opt {
                    name: opt,
                    value: Some(Operand::Lit(Value::Int(n))),
                } if ranks && *n > 0 && (opt == "--rounds" || opt == "-n") => {
                    rounds = *n as usize;
                }
                Arg::Opt { name: opt, .. } if ranks && opt == "--teleport" => {
                    return Err(
                        "graph:pagerank: '--teleport' is reserved for the personalised rank and not built yet"
                            .into(),
                    );
                }
                Arg::Opt { name: opt, .. } => {
                    return Err(if distance {
                        format!(
                            "graph:distance: '{opt}' is no option; the options are '--from=NODES' and '--weight=UNIT | ::prop'"
                        )
                    } else if kcore {
                        format!("graph:kcore: '{opt}' is no option; it takes the hop alone")
                    } else if conncomp {
                        format!("graph:conncomp: '{opt}' is no option; it takes the hop alone")
                    } else if betweenness {
                        format!(
                            "graph:betweenness: '{opt}' is no option; the options are '--normalized' and '--weight=UNIT | ::prop'"
                        )
                    } else if ranks {
                        format!(
                            "graph:pagerank: '{opt}' is no option; the options are '--damping=d', '--rounds=n' and '--weight=UNIT | ::prop'"
                        )
                    } else {
                        format!(
                            "edges: '{opt}' is no option; the option is '--weight=UNIT | ::prop' (-w=…)"
                        )
                    });
                }
                Arg::Expr(Operand::Rel {
                    steps,
                    projection: None,
                    ..
                }) if spec.hops.is_empty() => match steps.as_slice() {
                    [PathElem::Step(step)] => spec.hops.push(hop_of(step)?),
                    // `(->road|->rail)`: the alternatives of a plain
                    // group, each one hop — the table forms; the
                    // library takes one hop (label algebra inside it)
                    [PathElem::Group(g)]
                        if !library && g.quant.min == 1 && g.quant.max == Some(1) =>
                    {
                        for alt in &g.alts {
                            let [PathElem::Step(step)] = alt.as_slice() else {
                                return Err(hop_message());
                            };
                            spec.hops.push(hop_of(step)?);
                        }
                    }
                    [PathElem::Group(_)] if library => {
                        return Err(format!(
                            "{shown} takes one hop (->link, --road, ->*, ~*, ->(a || b)); write the labels' algebra inside it, not an alternation of hops"
                        ));
                    }
                    _ => return Err(hop_message()),
                },
                _ => return Err(hop_message()),
            }
        }
        if spec.hops.is_empty() {
            spec.hops.push(Hop {
                collect: Collect::Out,
                label: None,
                labels: Vec::new(),
            });
        }
        if distance {
            let Some(from) = from else {
                return Err(
                    "graph:distance takes its seeds: --from=NODES (a path from the root, a mark $$.S, an id anchor ((!7)))"
                        .into(),
                );
            };
            return Ok(GraphTable::Distance { spec, from });
        }
        if kcore {
            return Ok(GraphTable::Kcore { spec });
        }
        if betweenness {
            return Ok(GraphTable::Betweenness { spec, normalized });
        }
        if conncomp {
            return Ok(GraphTable::Conncomp { spec });
        }
        Ok(if ranks {
            GraphTable::Pagerank {
                spec,
                damping,
                rounds,
            }
        } else {
            GraphTable::Edges(spec)
        })
    }
}

fn hop_message() -> String {
    "edges takes the collecting hop as its operand: edges(->road) the outgoing road edges, \
     edges(<-road) the incoming, edges(--road) either way, edges(~*) every edge the navop \
     star walks; bare edges is edges(->*)"
        .to_string()
}

/// One stored edge of the induced subgraph.
pub(crate) struct Row {
    from: NodeId,
    to: NodeId,
    label: String,
    /// A tree edge carries no properties and no weight.
    tree: bool,
    /// The edge's identity where the edge is a row (spec: Edge rows):
    /// two rows on one leg are two rows here.
    row: Option<NodeId>,
}

impl Row {
    fn ctx(&self) -> EdgeCtx {
        EdgeCtx {
            source: self.from,
            label: self.label.clone(),
            target: self.to,
            row: self.row,
        }
    }
}

/// The stored edges of the induced subgraph of `nodes` that the
/// spec's hops collect, each once, in document order; whether the
/// collection is directed (every hop out or in).
fn collect_rows(adapter: &impl AstAdapter, nodes: &[NodeId], spec: &EdgesSpec) -> (Vec<Row>, bool) {
    let inside: HashSet<NodeId> = nodes.iter().copied().collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: HashSet<(NodeId, String, NodeId, Option<NodeId>)> = HashSet::new();
    let mut push = |from: NodeId, label: String, to: NodeId, tree: bool, row: Option<NodeId>| {
        if seen.insert((from, label.clone(), to, row)) {
            rows.push(Row {
                from,
                to,
                label,
                tree,
                row,
            });
        }
    };
    for &n in nodes {
        for hop in &spec.hops {
            if matches!(hop.collect, Collect::Out | Collect::Both | Collect::Any) {
                for (label, t, row) in adapter.edge_rows(n) {
                    if hop.admits(&label)
                        && inside.contains(&t)
                        && !(hop.wild() && adapter.link_consumed(n, &label))
                    {
                        push(n, label, t, false, row);
                    }
                }
            }
            if matches!(hop.collect, Collect::In | Collect::Both | Collect::Any) {
                for (label, s, row) in adapter.backlink_rows(n) {
                    if hop.admits(&label)
                        && inside.contains(&s)
                        && !(hop.wild() && adapter.link_consumed(s, &label))
                    {
                        push(s, label, n, false, row);
                    }
                }
            }
            if hop.collect == Collect::Any && hop.admits(CHILD) {
                for c in adapter.children(n) {
                    if inside.contains(&c) {
                        push(n, CHILD.to_string(), c, true, None);
                    }
                }
                if let Some(p) = adapter.parent(n)
                    && inside.contains(&p)
                {
                    push(p, CHILD.to_string(), n, true, None);
                }
            }
        }
    }
    let directed = spec
        .hops
        .iter()
        .all(|h| matches!(h.collect, Collect::Out | Collect::In));
    (rows, directed)
}

/// `@| edges`: the induced subgraph of `nodes` as a table.
pub(crate) fn edges_table(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
) -> Result<Table, String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec);
    // the edges' own properties: every name any edge carries, in
    // first-seen order
    let mut props: Vec<String> = Vec::new();
    let mut names: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.tree {
            continue;
        }
        let ns = r.ctx().property_names(adapter);
        for n in &ns {
            if !props.contains(n) {
                props.push(n.clone());
            }
        }
        names.insert(i, ns);
    }
    let weights = match &spec.weight {
        Some(w) => Some(edge_weights(adapter, &rows, w)?),
        None => None,
    };
    let mut header = vec![
        "source".to_string(),
        "target".to_string(),
        "label".to_string(),
        "directed".to_string(),
    ];
    header.extend(props.iter().cloned());
    if weights.is_some() {
        header.push("weight".to_string());
    }
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let mut cells = vec![
            node_ref(adapter, r.from),
            node_ref(adapter, r.to),
            Value::Str(r.label.clone()),
            Value::Bool(directed),
        ];
        let carried = names.get(&i);
        for p in &props {
            let v = match carried {
                Some(ns) if ns.contains(p) => r.ctx().property(adapter, p).unwrap_or(Value::Null),
                _ => Value::Null,
            };
            cells.push(v);
        }
        if let Some(ws) = &weights {
            cells.push(ws[i].0.clone());
        }
        out.push(cells);
    }
    Ok(Table::new(Some(header), out))
}

/// The weight of every row under `--weight` (spec: The graph as a
/// table): the cell the table shows and the number the algorithms
/// take — a UNIT selects the property of its dimension and converts
/// it (a tree edge has none: null, and no weight), `::prop` the
/// property as it stands (its number, or a quantity's base value;
/// a cell that is no number weighs nothing).
pub(crate) fn edge_weights(
    adapter: &impl AstAdapter,
    rows: &[Row],
    weight: &Weight,
) -> Result<Vec<(Value, Option<f64>)>, String> {
    match weight {
        Weight::Unit(unit) => {
            let (bv, base, mag, written) =
                crate::value::quantity::parse_unit_text(&format!("1 {}", unit.replace('_', " ")))
                    .ok_or_else(|| format!("'{unit}' is no unit the run knows (--weight=km)"))?;
            let b = BudgetBounds {
                floor: 0.0,
                ceiling: None,
                base,
                unit: written,
                scale: bv / mag,
                written: unit.clone(),
            };
            rows.iter()
                .map(|r| {
                    let w = if r.tree {
                        None
                    } else {
                        b.weight(adapter, &r.ctx())?
                    };
                    Ok(match w {
                        Some(w) => (
                            Value::Quantity {
                                value: w,
                                base: b.base.clone(),
                                written: Some((w / b.scale, b.unit.clone())),
                            },
                            Some(w),
                        ),
                        None => (Value::Null, None),
                    })
                })
                .collect()
        }
        Weight::Property(prop) => Ok(rows
            .iter()
            .map(|r| {
                let v = if r.tree {
                    None
                } else {
                    r.ctx().property(adapter, prop)
                };
                let n =
                    v.as_ref().and_then(|v| match v {
                        Value::Int(i) => Some(*i as f64),
                        Value::Float(f) => Some(*f),
                        Value::Quantity { value, .. } => Some(*value),
                        Value::Str(t) => t.trim().parse::<f64>().ok().or_else(|| {
                            crate::value::quantity::parse_unit_text(t).map(|(bv, ..)| bv)
                        }),
                        _ => None,
                    });
                (v.unwrap_or(Value::Null), n)
            })
            .collect()),
    }
}

/// `@| nodes`: the context's nodes as a table (`node` the reference,
/// `name`, `kind`) — and the regulae attached to them as columns
/// (spec: Attached Regulae), one per name in first-attach order over
/// the context, null where a node carries nothing under it.
pub(crate) fn nodes_table(adapter: &impl AstAdapter, nodes: &[NodeId]) -> Table {
    let attached: Vec<Vec<(String, Value)>> = if crate::pipeline::any_attached::<NodeId>() {
        nodes
            .iter()
            .map(crate::pipeline::attached_fields::<NodeId>)
            .collect()
    } else {
        vec![Vec::new(); nodes.len()]
    };
    let mut names: Vec<String> = Vec::new();
    for fields in &attached {
        for (k, _) in fields {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
    }
    let rows = nodes
        .iter()
        .zip(&attached)
        .map(|(&n, fields)| {
            let mut row = vec![
                node_ref(adapter, n),
                adapter.name(n).map_or(Value::Null, Value::Str),
                core_meta(adapter, n, "kind").unwrap_or(Value::Null),
            ];
            for k in &names {
                row.push(
                    fields
                        .iter()
                        .find(|(n, _)| n == k)
                        .map_or(Value::Null, |(_, v)| v.clone()),
                );
            }
            row
        })
        .collect();
    let mut header = vec!["node".to_string(), "name".to_string(), "kind".to_string()];
    header.extend(names);
    Table::new(Some(header), rows)
}

/// The rank of each node of the context over the induced subgraph by
/// the hops' edges (directed as written, an undirected hop both
/// ways), one per node in the order given (spec: The graph library):
/// the power iteration of `quarb-lib-graph`, the dangling mass
/// spread uniformly, a self-loop ignored, a repeated edge once, or
/// its weights added under `--weight`.
pub(crate) fn pagerank_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    damping: f64,
    rounds: usize,
) -> Result<Vec<f64>, String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec);
    let idx: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let weights = match &spec.weight {
        Some(w) => Some(edge_weights(adapter, &rows, w)?),
        None => None,
    };
    let mut triples: Vec<(usize, usize, Option<f64>)> = Vec::with_capacity(rows.len() * 2);
    for (i, r) in rows.iter().enumerate() {
        let (Some(&a), Some(&b)) = (idx.get(&r.from), idx.get(&r.to)) else {
            continue;
        };
        let w = match &weights {
            // a weightless edge of a weighted graph weighs nothing
            Some(ws) => match ws[i].1 {
                Some(w) => Some(w),
                None => continue,
            },
            None => None,
        };
        triples.push((a, b, w));
        if !directed {
            triples.push((b, a, w));
        }
    }
    if let Some(w) = triples.iter().find_map(|(_, _, w)| w.filter(|w| *w < 0.0)) {
        return Err(format!("pagerank takes no negative weight ({w})"));
    }
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(quarb_lib_graph::pagerank(&edges, damping, rounds))
}

/// The induced subgraph's edges as index triples over `nodes`, with
/// the weight per edge under `--weight` (a weightless edge of a
/// weighted graph is left out), an undirected collection both ways;
/// and the weight cells, for the shape a sum prints in.
fn index_edges(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
) -> Result<(Vec<(usize, usize, Option<f64>)>, Vec<Value>, bool), String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec);
    let idx: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let weights = match &spec.weight {
        Some(w) => Some(edge_weights(adapter, &rows, w)?),
        None => None,
    };
    let mut triples = Vec::with_capacity(rows.len() * 2);
    let mut cells = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let (Some(&a), Some(&b)) = (idx.get(&r.from), idx.get(&r.to)) else {
            continue;
        };
        let w = match &weights {
            Some(ws) => match ws[i].1 {
                Some(w) => {
                    cells.push(ws[i].0.clone());
                    Some(w)
                }
                None => continue,
            },
            None => None,
        };
        triples.push((a, b, w));
        if !directed {
            triples.push((b, a, w));
        }
    }
    Ok((triples, cells, directed))
}

/// `@| graph:distance`: each node's shortest distance from the seeds
/// (spec: The graph library) — an integer in hops; under
/// `--weight=UNIT` a quantity in that unit; under `--weight=::prop`
/// the property's values summed, in their own shape (integers stay
/// integers, quantities of one dimension a quantity, else a float).
/// A seed reads zero, a node no seed reaches null; a seed outside
/// the context counts for nothing.
pub(crate) fn distance_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    seeds: &[NodeId],
) -> Result<Vec<Value>, String> {
    let (triples, cells, _) = index_edges(adapter, nodes, spec)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    let seeds: Vec<usize> = seeds
        .iter()
        .filter_map(|s| nodes.iter().position(|n| n == s))
        .collect();
    let dist =
        quarb_lib_graph::distance(&edges, &seeds).map_err(|e| format!("graph:distance: {e}"))?;
    let shape = |d: f64| -> Value {
        match &spec.weight {
            None => Value::Int(d.round() as i64),
            Some(Weight::Unit(unit)) => {
                let (bv, base, mag, written) = crate::value::quantity::parse_unit_text(&format!(
                    "1 {}",
                    unit.replace('_', " ")
                ))
                .expect("checked by edge_weights");
                Value::Quantity {
                    value: d,
                    base,
                    written: Some((d / (bv / mag), written)),
                }
            }
            Some(Weight::Property(_)) => {
                if cells.iter().all(|c| matches!(c, Value::Int(_))) {
                    Value::Int(d.round() as i64)
                } else if let Some(Value::Quantity { base, .. }) = cells.first()
                    && cells
                        .iter()
                        .all(|c| matches!(c, Value::Quantity { base: b, .. } if b == base))
                {
                    Value::Quantity {
                        value: d,
                        base: base.clone(),
                        written: None,
                    }
                } else {
                    Value::Float(d)
                }
            }
        }
    };
    Ok(dist
        .into_iter()
        .map(|d| d.map_or(Value::Null, shape))
        .collect())
}

/// `@| graph:kcore`: each node's core number (spec: The graph
/// library) over the distinct neighbours by the hop's edges, either
/// direction, an integer per node.
pub(crate) fn kcore_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(quarb_lib_graph::kcore(&edges)
        .into_iter()
        .map(|k| Value::Int(k as i64))
        .collect())
}

/// `@| graph:betweenness`: each node's betweenness centrality (spec:
/// The graph library), Brandes over the induced subgraph — every
/// ordered pair of other nodes under a directed hop, every unordered
/// pair under an undirected one; `--normalized` divides by the
/// number of pairs; `--weight` makes the shortest paths the shortest
/// by weight.
pub(crate) fn betweenness_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    normalized: bool,
) -> Result<Vec<Value>, String> {
    let (triples, _, directed) = index_edges(adapter, nodes, spec)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    let cb = quarb_lib_graph::betweenness(&edges, spec.weight.is_some())
        .map_err(|e| format!("graph:betweenness: {e}"))?;
    let n = nodes.len() as f64;
    let halve = if directed { 1.0 } else { 0.5 };
    let pairs = if directed {
        (n - 1.0) * (n - 2.0)
    } else {
        (n - 1.0) * (n - 2.0) / 2.0
    };
    let scale = if normalized && pairs > 0.0 {
        halve / pairs
    } else {
        halve
    };
    Ok(cb.into_iter().map(|c| Value::Float(c * scale)).collect())
}

/// `@| graph:conncomp`: each node's connected component (spec: The
/// graph library), numbered from 1 in the order of the components'
/// first nodes — the strongly connected components under a directed
/// hop, the connected components under an undirected one.
pub(crate) fn conncomp_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(quarb_lib_graph::conncomp(&edges)
        .into_iter()
        .map(|c| Value::Int(c as i64))
        .collect())
}
