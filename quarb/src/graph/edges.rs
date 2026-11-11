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
    /// `~label`: every edge the navop wildcard walks, tree edges
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
    /// The step as written when it carries predicates or traits
    /// (spec: The graph library, draft.191): a hop operand takes
    /// exactly the predicate a walk's hop takes, evaluated once per
    /// edge with the node arrived at as the topic and `$-` the edge.
    pub filter: Option<Box<crate::syntax::ast::Step>>,
}

impl Hop {
    /// A bare wildcard (`->*`, `--*`, `~*`): the hop that skips the
    /// references an edge declaration consumed (spec: Edge rows).
    fn wild(&self) -> bool {
        self.label.is_none() && self.labels.is_empty()
    }

    /// Whether the hop's explicit label admits `label`.
    fn named(&self, label: &str) -> bool {
        self.label.as_deref().is_none_or(|l| l == label)
    }

    /// The edges of `edges` (label, other end, row) the hop walks,
    /// read as the crosslink hop reads its label algebra over
    /// single-label edges (spec: the edge model): a positive clause
    /// a class of edges, the conjunction of classes the edges of any
    /// class whose other end every class reaches, a wholly negated
    /// clause the complement among the edges themselves.
    fn select<'a>(
        &self,
        edges: &'a [(String, NodeId, Option<NodeId>)],
    ) -> Vec<&'a (String, NodeId, Option<NodeId>)> {
        let fits = |c: &LabelClause, l: &str| c.matches(std::slice::from_ref(&l.to_string()));
        let (negative, positive): (Vec<_>, Vec<_>) = self
            .labels
            .iter()
            .partition(|c| c.alts.iter().all(|a| a.neg));
        let class_ends: Vec<Vec<NodeId>> = positive
            .iter()
            .map(|c| {
                edges
                    .iter()
                    .filter(|(l, _, _)| fits(c, l))
                    .map(|(_, n, _)| *n)
                    .collect()
            })
            .collect();
        edges
            .iter()
            .filter(|(l, n, _)| {
                self.named(l)
                    && negative.iter().all(|c| fits(c, l))
                    && (positive.is_empty()
                        || (positive.iter().any(|c| fits(c, l))
                            && class_ends.iter().all(|ends| ends.contains(n))))
            })
            .collect()
    }
}

/// The `edges` call read: the hops (one, or the alternatives of a
/// group, `(->road|->rail)`) and the `--weight` option.
#[derive(Clone, Debug)]
pub struct EdgesSpec {
    pub hops: Vec<Hop>,
    pub weight: Option<Weight>,
}

/// `--weight=UNIT` (spec: The graph as a table): the unit selects
/// the edge weight of that unit's dimension, as the model declares
/// it, and converts it, as a budget does. The `::prop` form is
/// retired (the edge model, ruling 2026-11-07) and refused.
#[derive(Clone, Debug, PartialEq)]
pub enum Weight {
    /// `--weight=UNIT`: the edges' declared weight of that unit's
    /// dimension (spec: the edge model) — a property is no weight
    /// unless the model declares it one.
    Unit(String),
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
        /// `--edges`: the edges table with a `betweenness` column
        /// instead of a value per node.
        edges: bool,
    },
    /// `@| graph:conncomp(--*)`: each node's connected component,
    /// numbered from 1 by the components' first nodes.
    Conncomp {
        spec: EdgesSpec,
    },
    /// `@| graph:artpoints(--*)`: whether removing the node
    /// disconnects the graph, the edges read without direction.
    Artpoints {
        spec: EdgesSpec,
    },
    /// `@| graph:bridges(--*)`: the edges table kept to the edges
    /// whose removal disconnects the graph, read without direction.
    Bridges {
        spec: EdgesSpec,
    },
    /// `@| graph:route(->*; --from=NODES; --weight=…)`: each node's
    /// shortest route from the seeds as the list of nodes in order.
    Route {
        spec: EdgesSpec,
        from: Operand,
    },
    /// `@| graph:closeness(->*; --harmonic --weight=…)`: each node's
    /// closeness centrality, Wasserman and Faust's or the harmonic.
    Closeness {
        spec: EdgesSpec,
        harmonic: bool,
    },
    /// `@| graph:communities(--*; --resolution=g --weight=…)`: each
    /// node's community by the Leiden algorithm, numbered from one.
    Communities {
        spec: EdgesSpec,
        resolution: f64,
    },
    /// `@| graph:modularity(--*; --by=:.c --resolution=g --weight=…)`:
    /// one float for the context, the modularity of the partition the
    /// attached regula `--by` names.
    Modularity {
        spec: EdgesSpec,
        resolution: f64,
        by: Operand,
    },
    /// `@| graph:triangles(--*; --coefficient)`: each node's triangle
    /// count, or its local clustering coefficient.
    Triangles {
        spec: EdgesSpec,
        coefficient: bool,
    },
}

/// A graph a source declares it maintains measures over (spec: Graph
/// Declarations): the scope path, the hop, and each measure as
/// written in the declaration (`pagerank`,
/// `betweenness(--normalized)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphDeclaration {
    pub scope: String,
    pub hop: String,
    pub measures: Vec<String>,
}

fn hop_of(step: &crate::syntax::ast::Step) -> Result<Hop, String> {
    let filter =
        (!step.predicates.is_empty() || !step.traits.is_empty()).then(|| Box::new(step.clone()));
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
        filter,
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
        let artpoints = name == "graph:artpoints";
        let bridges = name == "graph:bridges";
        let route = name == "graph:route";
        let closeness = name == "graph:closeness";
        let communities = name == "graph:communities";
        let modularity = name == "graph:modularity";
        let triangles = name == "graph:triangles";
        let seeded = distance || route;
        let undirected = artpoints || bridges;
        let shown = name;
        let mut from: Option<Operand> = None;
        let mut by: Option<Operand> = None;
        let mut normalized = false;
        let mut edges_out = false;
        let mut harmonic = false;
        let mut coefficient = false;
        let mut resolution = 1.0f64;
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
                } if seeded && (opt == "--from" || opt == "-f") => {
                    if !matches!(
                        op,
                        Operand::Rel {
                            projection: None,
                            ..
                        }
                    ) {
                        return Err(format!(
                            "{shown}: --from takes a node operand (a path from the root, a mark $$.S, an id anchor ((!7)))"
                        ));
                    }
                    from = Some(op.clone());
                }
                Arg::Opt { name: opt, .. } if undirected && (opt == "--weight" || opt == "-w") => {
                    return Err(format!(
                        "{shown} reads the edges without their direction and takes no weight"
                    ));
                }
                Arg::Opt { name: opt, .. } if triangles && (opt == "--weight" || opt == "-w") => {
                    return Err(
                        "graph:triangles counts distinct neighbours and takes no weight".into(),
                    );
                }
                Arg::Opt {
                    name: opt,
                    value: None,
                } if closeness && (opt == "--harmonic" || opt == "-H") => {
                    harmonic = true;
                }
                Arg::Opt {
                    name: opt,
                    value: None,
                } if triangles && (opt == "--coefficient" || opt == "-c") => {
                    coefficient = true;
                }
                Arg::Opt {
                    name: opt,
                    value: Some(Operand::Lit(v)),
                } if (communities || modularity) && (opt == "--resolution" || opt == "-r") => {
                    resolution = number(v).filter(|r| *r > 0.0).ok_or_else(|| {
                        format!("{shown}: --resolution takes a positive number, '--resolution=1'")
                    })?;
                }
                Arg::Opt {
                    name: opt,
                    value: Some(op),
                } if modularity && (opt == "--by" || opt == "-b") => {
                    by = Some(op.clone());
                }
                Arg::Opt {
                    name: opt,
                    value: None,
                } if betweenness && (opt == "--edges" || opt == "-e") => {
                    edges_out = true;
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
                // `--weight=::prop` is retired (spec: the edge model,
                // ruling 2026-11-07): a property is a weight only where
                // the model declares it one, and a weight is read by
                // its unit; a count of hops is the count form
                Arg::Opt {
                    name: opt,
                    value:
                        Some(Operand::Rel {
                            steps,
                            projection: Some(Projection::Property(Some(prop))),
                            anchor: Anchor::Current,
                        }),
                } if (opt == "--weight" || opt == "-w") && steps.is_empty() => {
                    return Err(format!(
                        "{shown}: --weight=::{prop} names a property, and a property is no weight: \
                         declare it one in the model ('edge …: ::a -- ::b weights(::{prop});') and \
                         read it by its unit (--weight=km); a count of hops needs no weight"
                    ));
                }
                Arg::Opt { name: opt, .. } if opt == "--weight" || opt == "-w" => {
                    return Err(format!(
                        "{shown}: --weight takes a unit (--weight=km), the edges' declared weight of \
                         that dimension"
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
                            "graph:distance: '{opt}' is no option; the options are '--from=NODES' and '--weight=UNIT'"
                        )
                    } else if kcore {
                        format!("graph:kcore: '{opt}' is no option; it takes the hop alone")
                    } else if conncomp {
                        format!("graph:conncomp: '{opt}' is no option; it takes the hop alone")
                    } else if betweenness {
                        format!(
                            "graph:betweenness: '{opt}' is no option; the options are '--normalized', '--edges' and '--weight=UNIT'"
                        )
                    } else if undirected {
                        format!("{shown}: '{opt}' is no option; it takes the hop alone")
                    } else if route {
                        format!(
                            "graph:route: '{opt}' is no option; the options are '--from=NODES' and '--weight=UNIT'"
                        )
                    } else if closeness {
                        format!(
                            "graph:closeness: '{opt}' is no option; the options are '--harmonic' and '--weight=UNIT'"
                        )
                    } else if communities {
                        format!(
                            "graph:communities: '{opt}' is no option; the options are '--resolution=g' and '--weight=UNIT'"
                        )
                    } else if modularity {
                        format!(
                            "graph:modularity: '{opt}' is no option; the options are '--by=:.name', '--resolution=g' and '--weight=UNIT'"
                        )
                    } else if triangles {
                        format!(
                            "graph:triangles: '{opt}' is no option; the option is '--coefficient'"
                        )
                    } else if ranks {
                        format!(
                            "graph:pagerank: '{opt}' is no option; the options are '--damping=d', '--rounds=n' and '--weight=UNIT'"
                        )
                    } else {
                        format!("edges: '{opt}' is no option; the option is '--weight=UNIT' (-w=…)")
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
                filter: None,
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
        if route {
            let Some(from) = from else {
                return Err(
                    "graph:route takes its seeds: --from=NODES (a path from the root, a mark $$.S, an id anchor ((!7)))"
                        .into(),
                );
            };
            return Ok(GraphTable::Route { spec, from });
        }
        if kcore {
            return Ok(GraphTable::Kcore { spec });
        }
        if betweenness {
            return Ok(GraphTable::Betweenness {
                spec,
                normalized,
                edges: edges_out,
            });
        }
        if conncomp {
            return Ok(GraphTable::Conncomp { spec });
        }
        if artpoints {
            return Ok(GraphTable::Artpoints { spec });
        }
        if bridges {
            return Ok(GraphTable::Bridges { spec });
        }
        if closeness {
            return Ok(GraphTable::Closeness { spec, harmonic });
        }
        if communities {
            return Ok(GraphTable::Communities { spec, resolution });
        }
        if modularity {
            let Some(by) = by else {
                return Err(
                    "graph:modularity takes the partition: --by=:.name, the attached regula that names each node's part"
                        .into(),
                );
            };
            return Ok(GraphTable::Modularity {
                spec,
                resolution,
                by,
            });
        }
        if triangles {
            return Ok(GraphTable::Triangles { spec, coefficient });
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

impl GraphTable {
    /// Whether the command answers one table for the context rather
    /// than a value per node (spec: The graph library).
    pub(crate) fn answers_table(&self) -> bool {
        matches!(
            self,
            GraphTable::Edges(_)
                | GraphTable::Nodes
                | GraphTable::Bridges { .. }
                | GraphTable::Betweenness { edges: true, .. }
        )
    }

    /// Whether the command answers one value for the context (a
    /// reducing aggregate): modularity.
    pub(crate) fn answers_one(&self) -> bool {
        matches!(self, GraphTable::Modularity { .. })
    }

    /// The library command's name, `graph:` left off.
    pub(crate) fn command(&self) -> &'static str {
        match self {
            GraphTable::Edges(_) => "edges",
            GraphTable::Nodes => "nodes",
            GraphTable::Pagerank { .. } => "pagerank",
            GraphTable::Distance { .. } => "distance",
            GraphTable::Kcore { .. } => "kcore",
            GraphTable::Betweenness { .. } => "betweenness",
            GraphTable::Conncomp { .. } => "conncomp",
            GraphTable::Artpoints { .. } => "artpoints",
            GraphTable::Bridges { .. } => "bridges",
            GraphTable::Route { .. } => "route",
            GraphTable::Closeness { .. } => "closeness",
            GraphTable::Communities { .. } => "communities",
            GraphTable::Modularity { .. } => "modularity",
            GraphTable::Triangles { .. } => "triangles",
        }
    }

    /// The hop and the weight, for the commands that have them.
    pub(crate) fn spec(&self) -> Option<&EdgesSpec> {
        match self {
            GraphTable::Nodes => None,
            GraphTable::Edges(spec)
            | GraphTable::Pagerank { spec, .. }
            | GraphTable::Distance { spec, .. }
            | GraphTable::Kcore { spec }
            | GraphTable::Betweenness { spec, .. }
            | GraphTable::Conncomp { spec }
            | GraphTable::Artpoints { spec }
            | GraphTable::Bridges { spec }
            | GraphTable::Route { spec, .. }
            | GraphTable::Closeness { spec, .. }
            | GraphTable::Communities { spec, .. }
            | GraphTable::Modularity { spec, .. }
            | GraphTable::Triangles { spec, .. } => Some(spec),
        }
    }

    /// Whether the cost of the command is the product of nodes and
    /// edges rather than their sum (Brandes).
    pub(crate) fn quadratic(&self) -> bool {
        matches!(self, GraphTable::Betweenness { .. })
    }

    /// One text for the command, its hop and its options, equal
    /// exactly when a declared measure and a written command agree
    /// (spec: Graph Declarations).
    pub(crate) fn fingerprint(&self) -> String {
        let spec = self
            .spec()
            .map(|s| format!("{:?} {:?}", s.hops, s.weight))
            .unwrap_or_default();
        let options = match self {
            GraphTable::Pagerank {
                damping, rounds, ..
            } => format!("damping={damping} rounds={rounds}"),
            GraphTable::Distance { from, .. } | GraphTable::Route { from, .. } => {
                format!("from={from:?}")
            }
            GraphTable::Betweenness {
                normalized, edges, ..
            } => format!("normalized={normalized} edges={edges}"),
            GraphTable::Closeness { harmonic, .. } => format!("harmonic={harmonic}"),
            GraphTable::Communities { resolution, .. } => format!("resolution={resolution}"),
            GraphTable::Modularity { resolution, by, .. } => {
                format!("resolution={resolution} by={by:?}")
            }
            GraphTable::Triangles { coefficient, .. } => format!("coefficient={coefficient}"),
            _ => String::new(),
        };
        format!("{}|{spec}|{options}", self.command())
    }
}

fn hop_message() -> String {
    "edges takes the collecting hop as its operand: edges(->road) the outgoing road edges, \
     edges(<-road) the incoming, edges(--road) either way, edges(~*) every edge the navop \
     wildcard walks; bare edges is edges(->*)"
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
fn collect_rows(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    trace: &crate::graph::correlate::Trace,
) -> (Vec<Row>, bool) {
    let inside: HashSet<NodeId> = nodes.iter().copied().collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: HashSet<(NodeId, String, NodeId, Option<NodeId>)> = HashSet::new();
    // a candidate edge: the row and the node the hop arrives at,
    // the topic of the hop's predicate
    let mut push = |rows: &mut Vec<Row>,
                    from: NodeId,
                    label: String,
                    to: NodeId,
                    tree: bool,
                    row: Option<NodeId>| {
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
    let mut directed = true;
    for hop in &spec.hops {
        // The edges into a node, over a source that cannot walk its
        // links back, are read off the forward links of the set (the
        // user, 2026-11-10): the table holds the edges among the
        // nodes given, and every such edge leaves one of them, so
        // nothing is short. (The collection was refused, as the walk
        // forms `<-x`, `--x` are and stay: a walk leaves the set.)
        let incoming: Option<HashMap<NodeId, Vec<(String, NodeId, Option<NodeId>)>>> = {
            let named = if hop.labels.is_empty() {
                hop.label.as_deref()
            } else {
                None
            };
            (matches!(hop.collect, Collect::In | Collect::Both | Collect::Any)
                && nodes
                    .iter()
                    .any(|&n| adapter.backlinks_unsupported(n, named)))
            .then(|| {
                let mut into: HashMap<NodeId, Vec<(String, NodeId, Option<NodeId>)>> =
                    HashMap::new();
                for &s in nodes {
                    for (label, t, row) in adapter.edge_rows(s) {
                        if inside.contains(&t) {
                            into.entry(t).or_default().push((label, s, row));
                        }
                    }
                }
                into
            })
        };
        let mut found: Vec<(Row, NodeId)> = Vec::new();
        for &n in nodes {
            let from_here = found.len();
            if matches!(hop.collect, Collect::Out | Collect::Both | Collect::Any) {
                let rows = adapter.edge_rows(n);
                for (label, t, row) in hop.select(&rows).into_iter().cloned() {
                    if inside.contains(&t) && !(hop.wild() && adapter.link_consumed(n, &label)) {
                        found.push((
                            Row {
                                from: n,
                                to: t,
                                label,
                                tree: false,
                                row,
                            },
                            t,
                        ));
                    }
                }
            }
            if matches!(hop.collect, Collect::In | Collect::Both | Collect::Any) {
                let rows = match &incoming {
                    Some(into) => into.get(&n).cloned().unwrap_or_default(),
                    None => adapter.backlink_rows(n),
                };
                for (label, s, row) in hop.select(&rows).into_iter().cloned() {
                    if inside.contains(&s) && !(hop.wild() && adapter.link_consumed(s, &label)) {
                        found.push((
                            Row {
                                from: s,
                                to: n,
                                label,
                                tree: false,
                                row,
                            },
                            s,
                        ));
                    }
                }
            }
            if hop.collect == Collect::Any
                && hop.named(CHILD)
                && hop
                    .labels
                    .iter()
                    .all(|c| c.matches(std::slice::from_ref(&CHILD.to_string())))
            {
                for c in adapter.children(n) {
                    if inside.contains(&c) {
                        found.push((
                            Row {
                                from: n,
                                to: c,
                                label: CHILD.to_string(),
                                tree: true,
                                row: None,
                            },
                            c,
                        ));
                    }
                }
                if let Some(p) = adapter.parent(n)
                    && inside.contains(&p)
                {
                    found.push((
                        Row {
                            from: p,
                            to: n,
                            label: CHILD.to_string(),
                            tree: true,
                            row: None,
                        },
                        p,
                    ));
                }
            }
            // The hop's predicate and traits, as a walk applies them
            // from this node: the node arrived at is the topic, `$-`
            // the edge (spec: The graph library, draft.191), and a
            // positional predicate counts among this node's edges
            // (ruling 2026-11-10: `edges(->*[..2])` keeps two edges
            // per source node, as the hop keeps them in a path; the
            // predicates ran once over the edges of all the nodes
            // together, so it kept two edges in all).
            if let Some(step) = &hop.filter {
                let mut here = found.split_off(from_here);
                here.retain(|(_, at)| {
                    crate::graph::steps::trait_clauses_ok(adapter, *at, &step.traits)
                });
                if !step.predicates.is_empty() {
                    let state = crate::graph::navigate::GraphExt::at(n);
                    here = crate::graph::steps::apply_predicates(
                        adapter,
                        here,
                        step,
                        trace,
                        None,
                        &[],
                        &state,
                        &[],
                        |(_, at)| *at,
                        |(r, _)| Some(r.ctx()),
                    );
                }
                found.extend(here);
            }
        }
        for (r, _) in found {
            push(&mut rows, r.from, r.label, r.to, r.tree, r.row);
        }
        if !matches!(hop.collect, Collect::Out | Collect::In) {
            directed = false;
        }
    }
    (rows, directed)
}

/// `@| edges`: the induced subgraph of `nodes` as a table.
pub(crate) fn edges_table(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    trace: &crate::graph::correlate::Trace,
) -> Result<Table, String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec, trace);
    edges_table_of(adapter, spec, &rows, directed, &|_| true, None)
}

/// The edges table over the rows given, kept to those `keep` admits,
/// with one more column after the weight when `extra` names it: the
/// shape `edges` prints, which `bridges` and `betweenness(--edges)`
/// answer in (spec: The graph library).
fn edges_table_of(
    adapter: &impl AstAdapter,
    spec: &EdgesSpec,
    rows: &[Row],
    directed: bool,
    keep: &dyn Fn(usize) -> bool,
    extra: Option<(&str, &dyn Fn(usize) -> Value)>,
) -> Result<Table, String> {
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
        Some(w) => Some(edge_weights(adapter, rows, w)?),
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
    if let Some((name, _)) = extra {
        header.push(name.to_string());
    }
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if !keep(i) {
            continue;
        }
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
        if let Some((_, value)) = extra {
            cells.push(value(i));
        }
        out.push(cells);
    }
    Ok(Table::new(Some(header), out))
}

/// The rows' ends as indices into `nodes`, `None` for a row whose
/// end is outside (none, after `collect_rows`).
fn row_ends(nodes: &[NodeId], rows: &[Row]) -> Vec<Option<(usize, usize)>> {
    let idx: HashMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    rows.iter()
        .map(|r| Some((*idx.get(&r.from)?, *idx.get(&r.to)?)))
        .collect()
}

/// `graph:bridges`: the edges table kept to the bridges of the
/// undirected reading (spec: The graph library).
pub(crate) fn bridges_table(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    trace: &crate::graph::correlate::Trace,
) -> Result<Table, String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec, trace);
    let ends = row_ends(nodes, &rows);
    let edges = quarb_lib_graph::Edges::fold(
        nodes.len(),
        ends.iter().flatten().map(|&(a, b)| (a, b, None)),
    );
    let bridges: HashSet<(usize, usize)> = quarb_lib_graph::bridges(&edges).into_iter().collect();
    edges_table_of(
        adapter,
        spec,
        &rows,
        directed,
        &|i| ends[i].is_some_and(|(a, b)| bridges.contains(&(a.min(b), a.max(b)))),
        None,
    )
}

/// `graph:betweenness(--edges)`: the edges table with the share of
/// the shortest paths along each edge (spec: The graph library).
pub(crate) fn edge_betweenness_table(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    normalized: bool,
    trace: &crate::graph::correlate::Trace,
) -> Result<Table, String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec, trace);
    let ends = row_ends(nodes, &rows);
    let weights = match &spec.weight {
        Some(w) => Some(edge_weights(adapter, &rows, w)?),
        None => None,
    };
    let mut triples = Vec::with_capacity(rows.len() * 2);
    for (i, e) in ends.iter().enumerate() {
        let Some((a, b)) = *e else { continue };
        let w = match &weights {
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
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    let eb = quarb_lib_graph::edge_betweenness(&edges, spec.weight.is_some())
        .map_err(|e| format!("graph:betweenness: {e}"))?;
    let n = nodes.len() as f64;
    let pairs = if directed {
        n * (n - 1.0)
    } else {
        n * (n - 1.0) / 2.0
    };
    let halve = if directed { 1.0 } else { 0.5 };
    let scale = if normalized && pairs > 0.0 {
        halve / pairs
    } else {
        halve
    };
    let along = |a: usize, b: usize| -> Option<f64> {
        let k = edges.out(a).iter().position(|&(t, _)| t == b)?;
        Some(eb[a][k])
    };
    let value = |i: usize| -> Value {
        let Some((a, b)) = ends[i] else {
            return Value::Null;
        };
        if weights.as_ref().is_some_and(|ws| ws[i].1.is_none()) {
            return Value::Null;
        }
        let forward = along(a, b);
        let back = if directed { Some(0.0) } else { along(b, a) };
        match (forward, back) {
            (Some(f), Some(r)) => Value::Float((f + r) * scale),
            _ => Value::Null,
        }
    };
    edges_table_of(
        adapter,
        spec,
        &rows,
        directed,
        &|_| true,
        Some(("betweenness", &value)),
    )
}

/// `graph:artpoints`: whether each node cuts the undirected reading
/// of the graph (spec: The graph library).
pub(crate) fn artpoints_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(quarb_lib_graph::artpoints(&edges)
        .into_iter()
        .map(Value::Bool)
        .collect())
}

/// `graph:route`: each node's shortest route from the seeds as the
/// list of node references in order (spec: The graph library).
pub(crate) fn route_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    seeds: &[NodeId],
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    let seeds: Vec<usize> = seeds
        .iter()
        .filter_map(|s| nodes.iter().position(|n| n == s))
        .collect();
    let routes = quarb_lib_graph::route(&edges, &seeds, spec.weight.is_some())
        .map_err(|e| format!("graph:route: {e}"))?;
    Ok(routes
        .into_iter()
        .map(|r| match r {
            None => Value::Null,
            Some(path) => Value::List(std::sync::Arc::new(
                path.into_iter()
                    .map(|i| node_ref(adapter, nodes[i]))
                    .collect(),
            )),
        })
        .collect())
}

/// `graph:closeness`: each node's closeness along the hop as named
/// (spec: The graph library).
pub(crate) fn closeness_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    harmonic: bool,
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    // the distances in the unit named, as `distance` prints them, not
    // in the base unit the weights are folded in: closeness is a
    // ratio over their sum, so the unit shows in the answer
    let scale = match &spec.weight {
        Some(Weight::Unit(unit)) => {
            let (bv, _, mag, _) =
                crate::value::quantity::parse_unit_text(&format!("1 {}", unit.replace('_', " ")))
                    .expect("checked by edge_weights");
            mag / bv
        }
        _ => 1.0,
    };
    let triples: Vec<(usize, usize, Option<f64>)> = triples
        .into_iter()
        .map(|(a, b, w)| (a, b, w.map(|w| w * scale)))
        .collect();
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(
        quarb_lib_graph::closeness(&edges, spec.weight.is_some(), harmonic)
            .map_err(|e| format!("graph:closeness: {e}"))?
            .into_iter()
            .map(Value::Float)
            .collect(),
    )
}

/// `graph:communities`: each node's community by the Leiden
/// algorithm, numbered from one (spec: The graph library).
pub(crate) fn communities_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    resolution: f64,
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    if let Some(w) = triples.iter().find_map(|(_, _, w)| w.filter(|w| *w < 0.0)) {
        return Err(format!("graph:communities takes no negative weight ({w})"));
    }
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(quarb_lib_graph::communities(&edges, resolution)
        .into_iter()
        .map(|c| Value::Int(c as i64))
        .collect())
}

/// `graph:triangles`: each node's triangle count, or with
/// `--coefficient` its local clustering coefficient (spec: The
/// graph library).
pub(crate) fn triangles_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    coefficient: bool,
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(if coefficient {
        quarb_lib_graph::clustering(&edges)
            .into_iter()
            .map(Value::Float)
            .collect()
    } else {
        quarb_lib_graph::triangles(&edges)
            .into_iter()
            .map(|t| Value::Int(t as i64))
            .collect()
    })
}

/// `graph:modularity`: the modularity of the partition `part` (a
/// part index per node) over the hop's edges (spec: The graph
/// library), one float for the context.
pub(crate) fn modularity_value(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    resolution: f64,
    part: &[usize],
    trace: &crate::graph::correlate::Trace,
) -> Result<Value, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    if let Some(w) = triples.iter().find_map(|(_, _, w)| w.filter(|w| *w < 0.0)) {
        return Err(format!("graph:modularity takes no negative weight ({w})"));
    }
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(Value::Float(quarb_lib_graph::modularity(
        &edges, resolution, part,
    )))
}

/// The size of the induced subgraph a command computes over: the
/// context's distinct nodes and the edges among them (spec: The
/// graph library, the cost).
pub(crate) fn graph_size(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    trace: &crate::graph::correlate::Trace,
) -> (usize, usize) {
    let (rows, _) = collect_rows(adapter, nodes, spec, trace);
    (nodes.len(), rows.len())
}

/// The weight of every row under `--weight` (spec: The graph as a
/// table): the cell the table shows and the number the algorithms
/// take — a UNIT selects the property of its dimension and converts
/// it (a tree edge has none: null, and no weight).
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
                floor: None,
                ceiling: None,
                base,
                unit: written,
                scale: bv / mag,
                written: unit.clone(),
                rule: crate::syntax::ast::CostRule::Sum,
            };
            rows.iter()
                .map(|r| {
                    let w = if r.tree {
                        None
                    } else {
                        b.weight(adapter, &r.ctx(), true)?
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
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<f64>, String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec, trace);
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
    trace: &crate::graph::correlate::Trace,
) -> Result<(Vec<(usize, usize, Option<f64>)>, Vec<Value>, bool), String> {
    let (rows, directed) = collect_rows(adapter, nodes, spec, trace);
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
/// `--weight=UNIT` a quantity in that unit.
/// A seed reads zero, a node no seed reaches null; a seed outside
/// the context counts for nothing.
pub(crate) fn distance_values(
    adapter: &impl AstAdapter,
    nodes: &[NodeId],
    spec: &EdgesSpec,
    seeds: &[NodeId],
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _cells, _) = index_edges(adapter, nodes, spec, trace)?;
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
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
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
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, directed) = index_edges(adapter, nodes, spec, trace)?;
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
    trace: &crate::graph::correlate::Trace,
) -> Result<Vec<Value>, String> {
    let (triples, _, _) = index_edges(adapter, nodes, spec, trace)?;
    let edges = quarb_lib_graph::Edges::fold(nodes.len(), triples);
    Ok(quarb_lib_graph::conncomp(&edges)
        .into_iter()
        .map(|c| Value::Int(c as i64))
        .collect())
}
