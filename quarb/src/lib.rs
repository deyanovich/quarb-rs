//! The Quarb query engine.
//!
//! Quarb is a query language for *arbors* — tree-spanned graphs. This
//! crate is the engine: it lexes and parses a query, then evaluates
//! it against an [`AstAdapter`] that maps some data source onto the
//! arbor model.
//!
//! It currently implements tree navigation in full (child, descendant,
//! parent, ancestor, and sibling hops; proximal/distal reach; root and
//! leaf anchors; literal, glob, and `~(...)` regex name matching;
//! `<trait>` filters), scalar projection (`::` properties, `:::` core
//! metadata, `;;;` adapter metadata), `[...]` predicates (comparisons,
//! `and`/`or`/`not`/`!`, structural conditions, index selection),
//! `||` union, `->` / `<-` crosslink navigation, path patterns
//! (`(...)` groups with subpath alternation and `+`/`*`/`{m,n}`
//! quantifiers — simple-path expansion under the adapter's
//! [quantifier bound](AstAdapter::quantifier_bound)), and the register
//! system: per-capsa `| f` transforms, `@| f` whole-context
//! aggregation, `| .` push and `| $.` recall of breadcrumbs, and
//! `| .(expr)` subcontexts for grouped aggregation. It also does
//! correlation: `E1 <=> E2[…$*1…]` joins two contexts, a predicate
//! referencing a prior expression's context via `$*N`. It also
//! resolves cross-references: `::prop~>` maps a reference to its
//! target node (a JSON adapter follows a `$ref` JSON Pointer). The
//! reverse resolution `<~` and pattern search `=>` are not built yet.
//! See `doc/impl.tex`.
//!
//! ```
//! # use quarb::{AstAdapter, NodeId, QueryResult, run};
//! # struct Empty;
//! # impl AstAdapter for Empty {
//! #     fn root(&self) -> NodeId { NodeId(0) }
//! #     fn children(&self, _: NodeId) -> Vec<NodeId> { vec![] }
//! #     fn name(&self, _: NodeId) -> Option<String> { None }
//! # }
//! let hits = run("//*.rs", &Empty).unwrap();
//! assert!(matches!(hits, QueryResult::Nodes(ns) if ns.is_empty()));
//! ```

pub mod adapter;
mod ast;
mod encoding;
mod error;
mod exec;
mod lexer;
mod parser;
pub mod reflect;
pub use encoding::{base64, base64_decode, sha256, sha256_hex};
pub mod highlight;
pub mod quantity;
mod stdlib;
pub mod temporal;
mod unparse;
mod value;

pub use adapter::{AllowShell, AstAdapter, NodeId, QuantifierBound, WithNow};
pub use error::{QuarbError, Result};
pub use exec::QueryResult;
pub use parser::Defs;
pub use value::Value;

/// Lex, parse, and evaluate `query` against `adapter`.
///
/// Returns the [`QueryResult`] — a node set, or scalar values if the
/// query ends in a projection — or an error if the query is malformed
/// or uses a feature the engine does not implement yet.
pub fn run(query: &str, adapter: &impl AstAdapter) -> Result<QueryResult> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, Defs::default(), Some(adapter))?;
    exec::gate_shell(&ast, adapter)?;
    Ok(exec::eval(&ast, adapter))
}

/// Like [`run`], but returning the final capsae as `(node, topic)`
/// pairs, so each result value still knows which node produced it —
/// the provenance-bearing form used by typed emission (`qua --daiv`).
/// A `None` topic is a node result.
pub fn run_traced(query: &str, adapter: &impl AstAdapter) -> Result<Vec<(NodeId, Option<Value>)>> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, Defs::default(), Some(adapter))?;
    exec::gate_shell(&ast, adapter)?;
    Ok(exec::eval_traced(&ast, adapter))
}

/// Parse a definitions file — `def &name(params): body;` statements
/// only — into a fragment table for [`run_with_defs`]. Lines whose
/// first non-blank character is `#` are comments: a defs file is a
/// library, and a library wants a header. (Query text proper has no
/// comment syntax — queries are one-liners; the annotation lives in
/// the defs file.)
pub fn parse_defs(text: &str) -> Result<Defs> {
    let tokens = lexer::lex(&strip_defs_comments(text))?;
    parser::parse_defs(&tokens)
}

/// Blank out the `#` comment lines of a defs file, preserving line
/// structure. Session layers that *prepend defs text to query text*
/// (quai, the notebook kernels) must store the stripped form — the
/// query lexer itself has no comment syntax.
pub fn strip_defs_comments(text: &str) -> String {
    text.lines()
        .map(|l| if l.trim_start().starts_with('#') { "" } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Like [`run`], with a pre-seeded fragment table (a `--defs` file);
/// inline `def` statements extend it.
pub fn run_with_defs(query: &str, defs: &Defs, adapter: &impl AstAdapter) -> Result<QueryResult> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, defs.clone(), Some(adapter))?;
    exec::gate_shell(&ast, adapter)?;
    Ok(exec::eval(&ast, adapter))
}

/// Like [`run_traced`], with a pre-seeded fragment table.
pub fn run_traced_with_defs(
    query: &str,
    defs: &Defs,
    adapter: &impl AstAdapter,
) -> Result<Vec<(NodeId, Option<Value>)>> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, defs.clone(), Some(adapter))?;
    exec::gate_shell(&ast, adapter)?;
    Ok(exec::eval_traced(&ast, adapter))
}

/// Parse `query` (expanding defs) and render it back to canonical
/// query text — the expansion lens (`qua --expand`). Pure fragments
/// only: a data-aware macro (`&name!`) needs [`expand_with`].
pub fn expand(query: &str, defs: &Defs) -> Result<String> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_defs(&tokens, defs.clone())?;
    Ok(unparse::unparse(&ast))
}

/// Like [`expand`], with the dataset at hand, so data-aware macros
/// (`&name!`) can read it at expansion time.
pub fn expand_with(query: &str, defs: &Defs, adapter: &impl AstAdapter) -> Result<String> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, defs.clone(), Some(adapter))?;
    Ok(unparse::unparse(&ast))
}
