//! The syntax layer: the language as text and tree, no execution.
//! One grammar parses the calculator and the graph language, so the
//! layer sits at the bottom and knows nothing above the value
//! literals it builds.
pub mod ast;
pub(crate) mod expand;
pub mod highlight;
pub mod lexer;
pub mod parser;
pub mod registry;
pub mod unparse;
