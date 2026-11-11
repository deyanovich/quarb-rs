//! `graph:` — the graph library (spec: Aggregate Commands, "The
//! graph library"): the algorithms that read a whole graph and answer
//! a value per node, native to the engine, declared here as a command
//! library so the gate checks a call against its synopsis and `qua
//! man` lists it; the call itself never reaches `call`: every command
//! of the library is a row-aligned aggregate the pipeline runs over
//! the context (`graph_library_command`), its algorithm in
//! `quarb-lib-graph`.

use crate::pipeline::library::{
    CommandLibrary, InputSlot, LibCall, LibFailure, OperandSpec, OptionSpec, Pipe, Synopsis,
};
use crate::value::Value;

/// The library, one static the run options answer for the prefix.
pub struct GraphLibrary;

pub static GRAPH: GraphLibrary = GraphLibrary;

const HOP: OperandSpec = OperandSpec {
    name: "hop",
    doc: "the edges collected, as a hop: ->link, <-link, --road, ->*, ~*, ->(a || b); ->* when left out",
    values: &[],
    required: false,
};

const COMMANDS: &[Synopsis] = &[
    Synopsis {
        name: "pagerank",
        doc: "the rank of each node of the context over the induced subgraph by the hop's edges: from 1/N, n rounds of (1 - d)/N + d times the in-neighbours' rank over their out-degree, the dangling mass spread evenly; a self-loop ignored, a repeated edge once (its weights added under --weight); the ranks sum to one",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-d"),
                long: "damping",
                takes_value: true,
                doc: "the damping factor, 0.85 unless given",
            },
            OptionSpec {
                short: Some("-n"),
                long: "rounds",
                takes_value: true,
                doc: "the number of rounds, 20 unless given",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "the edge weight: a UNIT selects the weight of its dimension; each edge's share of its source's rank is proportional to its weight",
            },
            OptionSpec {
                short: None,
                long: "teleport",
                takes_value: true,
                doc: "reserved for the personalised rank; not built yet",
            },
        ],
        input: InputSlot::Context,
        returns: "float per node",
    },
    Synopsis {
        name: "distance",
        doc: "each node's shortest distance from the seed set along the hop's edges: an integer in hops, a quantity in the unit with --weight=UNIT; a seed reads 0, a node no seed reaches null, a seed outside the context counts for nothing",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-f"),
                long: "from",
                takes_value: true,
                doc: "the seeds: an anchored node operand — a path from the root (/airports/LJU), a mark ($$.S), an id anchor (((!7))); several seeds give the nearest",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "the edge weight summed along the way: a UNIT selects the weight of its dimension and converts it; a negative weight refuses",
            },
        ],
        input: InputSlot::Context,
        returns: "int, quantity or number per node (null where unreached)",
    },
    Synopsis {
        name: "kcore",
        doc: "each node's core number: the largest k such that the node is in the subgraph where every node keeps at least k neighbours, a neighbour counted once over the hop's edges in either direction; a node without neighbours is 0",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[],
        input: InputSlot::Context,
        returns: "int per node",
    },
    Synopsis {
        name: "betweenness",
        doc: "each node's betweenness centrality (Brandes): over every pair of other nodes, the share of the shortest paths between them that pass through the node, a pair with several shortest paths dividing its unit among them; each unordered pair once under an undirected hop, each ordered pair under a directed one; a node on no path between others is 0",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-N"),
                long: "normalized",
                takes_value: false,
                doc: "divide by the number of pairs ((n-1)(n-2)/2 undirected, (n-1)(n-2) directed), so 0..1",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "shortest by weight: a UNIT selects the weight of its dimension and converts it; a negative weight refuses",
            },
            OptionSpec {
                short: Some("-e"),
                long: "edges",
                takes_value: false,
                doc: "answer the table edges prints for the operand with a betweenness column: the share of the shortest paths that run along each edge, under the same --normalized and --weight",
            },
        ],
        input: InputSlot::Context,
        returns: "float per node, or the edges table with --edges",
    },
    Synopsis {
        name: "conncomp",
        doc: "each node's connected component, numbered from 1 in the order in which the components' first nodes stand in the context: the strongly connected components under a directed hop, the connected components under an undirected one (--*, ~*); a node without an edge is its own component",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[],
        input: InputSlot::Context,
        returns: "int per node",
    },
    Synopsis {
        name: "artpoints",
        doc: "true for each node whose removal disconnects the graph of the hop (an articulation point), false for every other: the edges are read without their direction, a repeated edge counts once and a self-loop never, a node without an edge is false",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[],
        input: InputSlot::Context,
        returns: "bool per node",
    },
    Synopsis {
        name: "bridges",
        doc: "the table edges prints for the same operand, kept to the rows whose removal disconnects the graph, read without direction as artpoints reads it; an empty table where no edge is a bridge",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[],
        input: InputSlot::Context,
        returns: "the edges table, the bridges only",
    },
    Synopsis {
        name: "route",
        doc: "each node's shortest route from the seeds along the hop's edges, the list of the nodes in order, the seed first and the node itself last, the seed's own route the list of itself; several seeds give the nearest, a node no seed reaches null, a seed outside the context counts for nothing; where several routes are shortest the one whose predecessor stands first in document order",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-f"),
                long: "from",
                takes_value: true,
                doc: "the seeds: an anchored node operand — a path from the root, a mark ($$.S), an id anchor (((!7)))",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "shortest by weight: a UNIT selects the weight of its dimension and converts it; a negative weight refuses",
            },
        ],
        input: InputSlot::Context,
        returns: "list of nodes per node (null where unreached)",
    },
    Synopsis {
        name: "closeness",
        doc: "each node's closeness centrality along the hop: with r the other nodes it reaches and n the context's nodes, r/(n-1) times r over the sum of the distances (Wasserman and Faust), 1 for a node adjacent to every other, 0 for one reaching none; the distances as distance measures them, in the hop's direction",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-H"),
                long: "harmonic",
                takes_value: false,
                doc: "the harmonic form instead: the sum of 1/d over the other nodes divided by n-1, an unreached node adding nothing",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "distances by weight: a UNIT selects the weight of its dimension and converts it; a negative weight refuses",
            },
        ],
        input: InputSlot::Context,
        returns: "float per node",
    },
    Synopsis {
        name: "communities",
        doc: "each node's community by the Leiden algorithm towards modularity, numbered from 1 in the order in which the communities' first nodes stand in the context: nodes moved in document order to the neighbouring community of the greatest gain, a tie to the earlier community, each community refined and the parts aggregated until nothing moves; the edges read without direction, a repeated edge adding its weight",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-r"),
                long: "resolution",
                takes_value: true,
                doc: "the resolution the modularity is taken at, 1 unless given; higher favours smaller communities",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "the edge weight: a UNIT selects the weight of its dimension; parallel edges add theirs",
            },
        ],
        input: InputSlot::Context,
        returns: "int per node",
    },
    Synopsis {
        name: "modularity",
        doc: "one float for the context: the modularity (Newman and Girvan) of the partition the attached regula named by --by gives, the share of the edges inside the parts minus the share a random graph of the same degrees would place there, in -1/2..1; a directed hop takes Leicht and Newman's directed form; a node with nothing attached under the name is a part of its own",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[
            OptionSpec {
                short: Some("-b"),
                long: "by",
                takes_value: true,
                doc: "the partition: the attached regula that names each node's part (:.c)",
            },
            OptionSpec {
                short: Some("-r"),
                long: "resolution",
                takes_value: true,
                doc: "scales the expected share, 1 unless given",
            },
            OptionSpec {
                short: Some("-w"),
                long: "weight",
                takes_value: true,
                doc: "the edge weight: a UNIT selects the weight of its dimension; parallel edges add theirs",
            },
        ],
        input: InputSlot::Context,
        returns: "float",
    },
    Synopsis {
        name: "triangles",
        doc: "each node's triangles: the pairs of its neighbours along the hop that are neighbours of each other, the edges read without direction, a repeated edge once and a self-loop never; the sum over the context is three times the graph's triangle count",
        pipes: Pipe::Agg,
        operands: &[HOP],
        options: &[OptionSpec {
            short: Some("-c"),
            long: "coefficient",
            takes_value: false,
            doc: "the local clustering coefficient instead (Watts and Strogatz): 2T/(k(k-1)) over the k distinct neighbours, 0 under two neighbours, in 0..1",
        }],
        input: InputSlot::Context,
        returns: "int per node, or float per node with --coefficient",
    },
];

impl CommandLibrary for GraphLibrary {
    fn prefix(&self) -> &str {
        "graph"
    }
    fn commands(&self) -> &[Synopsis] {
        COMMANDS
    }
    fn call(&self, name: &str, _call: LibCall) -> Result<Value, LibFailure> {
        // Reached only where the pipeline did not run the command
        // over the context: after `|`, or at the calculator.
        Err(LibFailure::Unknown(format!(
            "graph:{name} reads the context's nodes and the edges among them: write it after '@|' on a node context"
        )))
    }
}
