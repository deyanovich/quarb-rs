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
//! resolves cross-references: `::prop-->` maps a reference to its
//! target node (a JSON adapter follows a `$ref` JSON Pointer). The
//! reverse resolution `<--` and pattern search `=>` are not built yet.
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
pub mod complete;
pub mod highlight;
pub mod quantity;
mod stdlib;
pub mod temporal;
mod unparse;
mod value;

pub use adapter::{AllowShell, AstAdapter, NodeId, Provenance, QuantifierBound, WithNow};
pub use error::{QuarbError, Result};
pub mod koine;

/// The invocation instant, pinned once by the tool layer (`qua
/// --now`, or the clock read once at startup) and read by
/// adapters that would otherwise consult the wall clock when
/// resolving a relative window (`since=30m`). A pinned run
/// replays: evaluation never reads a clock, and neither does a
/// mount. `None` before the tool sets it — a library embedder
/// that never pins gets the old wall-clock behavior.
mod pinned {
    use std::cell::Cell;
    thread_local! {
        pub(super) static NOW: Cell<Option<(i64, u32)>> = const { Cell::new(None) };
    }
}

/// Pin the invocation instant for this thread (seconds and nanos
/// since the epoch). Called once, by the tool, before mounting.
pub fn set_invocation_instant(secs: i64, nanos: u32) {
    pinned::NOW.with(|c| c.set(Some((secs, nanos))));
}

/// The pinned invocation instant, if the tool layer set one.
pub fn invocation_instant() -> Option<(i64, u32)> {
    pinned::NOW.with(|c| c.get())
}

/// The pinned instant in whole seconds, falling back to the wall
/// clock — the one call an adapter needs when resolving a
/// relative window. Reading it in `open` (never during
/// evaluation) keeps a mount as reproducible as a query.
pub fn now_secs() -> i64 {
    match invocation_instant() {
        Some((secs, _)) => secs,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    }
}
pub use exec::QueryResult;
pub use parser::Defs;
pub use value::Value;

/// Convert a refusal recorded during evaluation into an error. The
/// eval layer below the entry points has no error channel; a walk
/// that would return a silently incomplete answer (e.g. a
/// quantifier cut off at the bound) records a refusal instead, and
/// the entry points surface it here.
fn checked<T>(result: T) -> Result<T> {
    match exec::take_refusal() {
        Some(msg) => Err(QuarbError::Refused(msg)),
        None => Ok(result),
    }
}

/// Lex, parse, and evaluate `query` against `adapter`.
///
/// Returns the [`QueryResult`] — a node set, or scalar values if the
/// query ends in a projection — or an error if the query is malformed
/// or uses a feature the engine does not implement yet.
pub fn run(query: &str, adapter: &impl AstAdapter) -> Result<QueryResult> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, Defs::default(), Some(adapter))?;
    exec::gate_shell(&ast, adapter)?;
    checked(exec::eval(&ast, adapter))
}

/// Like [`run`], but returning the final capsae as `(node, topic)`
/// pairs, so each result value still knows which node produced it —
/// the provenance-bearing form used by typed emission (`qua --daiv`).
/// A `None` topic is a node result.
pub fn run_traced(query: &str, adapter: &impl AstAdapter) -> Result<Vec<(NodeId, Option<Value>)>> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, Defs::default(), Some(adapter))?;
    exec::gate_shell(&ast, adapter)?;
    checked(exec::eval_traced(&ast, adapter))
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
    checked(exec::eval(&ast, adapter))
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
    checked(exec::eval_traced(&ast, adapter))
}

/// True if `query` (inline defs allowed) opens with an expression
/// head or a lone root anchor and never navigates a document: every
/// branch is a step-less, projection-less root anchor and there are
/// no correlations. Such a query can run against a bare root with
/// no mounted target — the calculator invocation (spec: The
/// Expression Head). Unparseable text answers `false`; the caller's
/// normal run path owns the error message.
pub fn is_calculator(query: &str) -> bool {
    let Ok(tokens) = lexer::lex(query) else {
        return false;
    };
    let Ok(ast) = parser::parse_with_defs(&tokens, Defs::default()) else {
        return false;
    };
    ast.correlations.is_empty()
        && !ast.branches.is_empty()
        && ast.branches.iter().all(|b| {
            matches!(b.anchor, ast::Anchor::Root) && b.steps.is_empty() && b.projection.is_none()
        })
}

/// Parse `query` (expanding defs) and render it back to canonical
/// query text — the expansion lens (`qua --expand`). Pure fragments
/// only: a data-aware macro (`&name!`) needs [`expand_with`].
pub fn expand(query: &str, defs: &Defs) -> Result<String> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_defs(&tokens, defs.clone())?;
    Ok(unparse::unparse(&ast))
}

/// The `macroexpand-1` lens (`qua --expand-1`): parse fully, but
/// return each directly-invoked macro's generated text before its
/// re-expansion — one entry per direct invocation, in source
/// order. Run the printed text again to take the next step;
/// [`expand`] is the fixed point.
pub fn expand_first(query: &str, defs: &Defs) -> Result<Vec<String>> {
    let tokens = lexer::lex(query)?;
    parser::parse_first_steps(&tokens, defs.clone(), None)
}

/// Like [`expand_first`], with the dataset at hand, so data-aware
/// macros (`&name!`) can read it at expansion time.
pub fn expand_first_with(
    query: &str,
    defs: &Defs,
    adapter: &impl AstAdapter,
) -> Result<Vec<String>> {
    let tokens = lexer::lex(query)?;
    parser::parse_first_steps(&tokens, defs.clone(), Some(adapter))
}

/// Like [`expand`], with the dataset at hand, so data-aware macros
/// (`&name!`) can read it at expansion time.
pub fn expand_with(query: &str, defs: &Defs, adapter: &impl AstAdapter) -> Result<String> {
    let tokens = lexer::lex(query)?;
    let ast = parser::parse_with_data(&tokens, defs.clone(), Some(adapter))?;
    Ok(unparse::unparse(&ast))
}
