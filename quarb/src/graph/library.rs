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
                doc: "the edge weight: a UNIT selects the property of its dimension, ::prop names it; each edge's share of its source's rank is proportional to its weight",
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
        doc: "each node's shortest distance from the seed set along the hop's edges: an integer in hops, a quantity in the unit with --weight=UNIT, the property's values summed with --weight=::prop; a seed reads 0, a node no seed reaches null, a seed outside the context counts for nothing",
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
                doc: "the edge weight summed along the way: a UNIT selects the property of its dimension and converts it, ::prop names it; a negative weight refuses",
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
                doc: "shortest by weight: a UNIT selects the property of its dimension and converts it, ::prop names it; a negative weight refuses",
            },
        ],
        input: InputSlot::Context,
        returns: "float per node",
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
