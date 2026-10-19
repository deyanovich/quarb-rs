//! The standard library layer: the functions over values, reached
//! by name from a call, with the arithmetic and the comparisons the
//! operators run. Above the syntax (a call is an AST node) and the
//! value space, below the thread and the pipeline that run them.
mod functions;
pub use crate::syntax::registry::{known_agg, known_keyed, known_scalar};
pub use functions::*;
pub(crate) mod arith;
pub(crate) use arith::UnitScale;
pub(crate) mod compare;
pub(crate) mod text;
