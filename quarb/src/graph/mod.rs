//! The graph layer: arbors, anchors, steps, adapters, correlation,
//! reflection and the trace. Everything that knows a node.
pub mod adapter;
pub mod complete;
pub mod conformance;
pub(crate) mod correlate;
pub(crate) mod host;
pub(crate) mod identity;
pub mod koine;
pub(crate) mod memo;
pub mod names;
pub(crate) mod navigate;
pub(crate) mod operand;
pub mod plan;
pub mod reflect;
pub mod run;
pub(crate) mod serialize;
pub mod source;
pub(crate) mod steps;
pub(crate) mod tabula;
pub mod target;
#[cfg(test)]
mod tests;
pub mod trace;

// The graph's namespace: the pipeline's, and every module of its own,
// so a module of the layer writes `use super::*;` and sees both.
pub(crate) use crate::pipeline::*;
pub(crate) use adapter::{AstAdapter, InstantFrom, NodeId, Provenance, ProvenanceList};
pub(crate) use correlate::*;
pub(crate) use host::*;
pub(crate) use identity::*;
pub(crate) use memo::*;
pub(crate) use navigate::*;
pub(crate) use operand::*;
pub(crate) use run::*;
pub(crate) use serialize::*;
pub(crate) use steps::*;
