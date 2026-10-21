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
//! `<trait>` filters), scalar projection (`::` properties, `:` core
//! metadata, `:::` adapter metadata), `[...]` predicates (comparisons,
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
//! target node (a JSON adapter follows a `$ref` JSON Pointer), the
//! reverse resolution `<--` (who refers here) and the value match
//! `=>`. See `doc/impl.tex`.
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

pub mod error;
mod graph;
mod pipeline;
mod stdlib;
mod syntax;
mod thread;
mod value;
// The public module paths, as they were before the layering.
pub use graph::complete;
pub use graph::host::{QueryResult, Traced, resolved_provenance};
pub use graph::identity::{canonical_identity, canonical_identity_at_open, is_url, name_path};
pub use graph::run::{Run, inherited};
pub use graph::tabula::{table_node_records, table_rows_of};
pub use graph::{adapter, conformance, koine, names, plan, reflect, source, target, trace};
#[cfg(feature = "kaiv")]
pub use pipeline::kaiv_out;
pub use pipeline::{env, library};
pub use quarb_time::{calendar, calnames, datenames, durnames, regional};
/// The temporal fragment (the `quarb-time` crate, at its old paths).
pub use quarb_time::{period, temporal, timex};
pub use stdlib::{tabulate, tabulate_in, tabulate_with};
use syntax::{ast, lexer, parser, unparse};
pub use syntax::{highlight, man, registry};
pub use thread::prov::{ORIGIN_CAP, Origin, Origins, Prov};
pub use value::encoding::{base64, base64_decode, sha256, sha256_hex};
pub use value::split_seconds_pub;
pub use value::table::{TABLE_CSS, table_cells, table_columns, table_html, table_html_plain};
pub use value::{bibref, encoding, locale, number, quantity, table, translit};

pub use error::{Kind as ErrorKind, QuarbError, Result};
pub use graph::adapter::{
    Ask, AstAdapter, Cap, Catalog, CatalogLookup, Cmp, Confidence, Conjunct, Estimate, ExternalRef,
    InstantFrom, Latency, LinkDir, NodeId, PlanAxis, Provenance, ProvenanceList, Selection,
    StepPlan, Unopened,
};

pub use pipeline::env::{EnvKind, Environment};
pub use pipeline::library::CommandLibrary;
pub use syntax::parser::{Defs, parse_scope_path};
/// The invocation instant, pinned once by the tool layer (`qua
/// --now`, or the clock read once at startup) and read by
/// adapters that would otherwise consult the wall clock when
/// resolving a relative window (`since=30m`). A pinned run
/// replays: evaluation never reads a clock, and neither does a
/// mount. `None` before the tool sets it — a library embedder
/// that never pins gets the old wall-clock behavior.
pub use value::ambient::{
    SentenceBonds, fetched, fetched_opt, invocation_instant, now_secs, record_refusal,
    sentence_bonds, set_invocation_instant, set_sentence_bonds,
};
pub use value::{ColumnType, Foreign, ForeignValue, NodeRef, Table, Value};

/// The universal search over a step body: `*//BODY`, with the
/// wildcard supplied when the body opens with a trait expression or
/// a predicate (`trait <long-text>: [:: | cc > 300];` means every
/// node that passes).
pub fn step_body_query(body: &str) -> String {
    let body = body.trim();
    if body.starts_with(['<', '[']) {
        format!("*//*{body}")
    } else {
        format!("*//{body}")
    }
}

/// One path step's body, parsed — a name, a pattern, an
/// alternation, a trait expression, predicates, in any combination,
/// without the navigation operator — as a model's `trait <name>:
/// BODY;` declaration takes it. Test a node against it with
/// [`node_matches_step_body`]: decided at the node, no walk.
pub struct StepBody(ast::PathElem);

/// Parse `body` as one step's body; refuses anything beyond that
/// single step, and positional predicates (`[1]`, `[2..3]`), which
/// have no meaning at a single node.
pub fn parse_step_body(body: &str) -> Result<StepBody> {
    let q = parser::parse(&lexer::lex(&step_body_query(body))?)?;
    let refuse = |what: &str| {
        Err(QuarbError::parse(format!(
            "a trait declaration takes one step's body (a name, a pattern, \
             a trait expression, predicates), not {what}: '{body}'"
        )))
    };
    if !q.correlations.is_empty() || !q.pipeline.is_empty() {
        return refuse("a pipeline");
    }
    if q.branches.len() != 1 || q.branches[0].projection.is_some() {
        return refuse("a projection or a union");
    }
    if q.branches[0].steps.len() != 1 {
        return refuse("a path");
    }
    let elem = q
        .branches
        .into_iter()
        .next()
        .unwrap()
        .steps
        .into_iter()
        .next()
        .unwrap();
    let preds = match &elem {
        ast::PathElem::Step(s) => &s.predicates,
        ast::PathElem::Group(g) => &g.predicates,
        _ => return refuse("that"),
    };
    if preds
        .iter()
        .any(|p| matches!(p, ast::Predicate::Index(_) | ast::Predicate::Range(_, _)))
    {
        return refuse("a positional predicate");
    }
    Ok(StepBody(elem))
}

/// Check that `body` parses as one step's body (see
/// [`parse_step_body`]).
pub fn validate_step_body(body: &str) -> Result<()> {
    parse_step_body(body).map(|_| ())
}

/// Whether `node` would match the step body as a step of a path:
/// decided at the node, the predicates evaluated there, no hop.
pub fn node_matches_step_body(adapter: &impl AstAdapter, node: NodeId, body: &StepBody) -> bool {
    graph::steps::node_matches_elem(adapter, node, &body.0)
}

/// Convert a refusal recorded during evaluation into an error. The
/// eval layer below the entry points has no error channel; a walk
/// that would return a silently incomplete answer (e.g. a
/// quantifier cut off at the bound) records a refusal instead, and
/// the entry points surface it here.
fn checked<T>(result: T) -> Result<T> {
    match value::ambient::take_refusal() {
        Some(msg) => Err(QuarbError::refused(msg)),
        None => Ok(result),
    }
}

/// The options a [`run`]-family entry runs under: the enclosing
/// run's when one is in progress (an adapter answering through the
/// engine mid-evaluation keeps its caller's instant, bound, gate
/// and targets), the ambient ones at the top.

/// A query's closing serialization for people — `@| csv` or
/// `@| table` as the last command — taken off for the tool to render.
#[derive(Debug, Clone)]
pub struct ClosingTable {
    pub csv: bool,
    /// The named columns, in order; empty for every field.
    pub columns: Vec<String>,
    /// The delimiter `csv(";")` names, when it names one.
    pub delimiter: Option<u8>,
}

/// What a run leaves for its caller.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// A node set, or scalar values if the query ends in a
    /// projection.
    pub result: QueryResult,
    /// The closing `@| csv` / `@| table` taken off for rendering
    /// ([`Query::for_rendering`]), with the rows standing in
    /// `result` as they were before it.
    pub closing: Option<ClosingTable>,
    /// The unresolved external references: the identifiers of
    /// documents the query referenced but could not reach (not
    /// mounted, no tree declared for them).
    pub refs: Vec<String>,
    /// The static plan the run was made under, when the run asked
    /// for it (`Run::plan`) or held it to a budget.
    pub plan: Option<plan::Plan>,
}

/// A parsed query, ready to run against an adapter under a
/// [`Run`]: parse once, run many.
#[derive(Debug, Clone)]
pub struct Query {
    ast: ast::Query,
    closing: Option<ClosingTable>,
}

/// Whether `text` is a query whose closing command is `@| table` —
/// the browser hosts' rule: such a result renders as the HTML grid
/// ([`table_html`]), everything else stays text. A query that does
/// not parse closes with nothing.
pub fn closes_with_table(text: &str) -> bool {
    lexer::lex(text)
        .and_then(|tokens| parser::parse(&tokens))
        .ok()
        .and_then(|q| match q.pipeline.last() {
            Some(ast::Command::Agg(call)) => Some(matches!(call.name.as_str(), "table" | "pretty")),
            _ => None,
        })
        .unwrap_or(false)
}

impl Query {
    /// Lex and parse `text` with the dataset at hand (data-aware
    /// macros read it at expansion time), under the ambient options.
    pub fn parse(text: &str, adapter: &impl AstAdapter) -> Result<Query> {
        Query::parse_with(text, &Defs::default(), adapter, &inherited())
    }

    /// [`Query::parse`] with a pre-seeded fragment table and the
    /// options a macro body expands under (its shell gate, its
    /// instant).
    pub fn parse_with(
        text: &str,
        defs: &Defs,
        adapter: &impl AstAdapter,
        run: &Run,
    ) -> Result<Query> {
        let _scope = graph::run::enter(Some(run.clone()));
        let (tokens, spans) = lexer::lex_spanned(text)?;
        let ast = parser::parse_with_data_spanned(
            &tokens,
            Some(&spans),
            defs.clone(),
            &graph::reflect::Reflector::new(Some(adapter)),
        )?;
        Ok(Query { ast, closing: None })
    }

    /// For a tool that renders under an output locale: when the
    /// query closes with `@| csv` or `@| table`, the run returns the
    /// rows as they stand before that command, with its shape in
    /// [`Outcome::closing`], and the tool tabulates them itself
    /// (`tabulate_in` with its locale's cells and delimiter). The
    /// command anywhere else, and every other command, yields the same
    /// values as under a plain run — a value never depends on a
    /// locale.
    pub fn for_rendering(mut self) -> Query {
        if let Some(ast::Command::Agg(call)) = self.ast.pipeline.last_mut()
            && matches!(call.name.as_str(), "csv" | "table" | "pretty")
        {
            let (delimiter, columns) = stdlib::split_csv_delimiter(call);
            self.closing = Some(ClosingTable {
                csv: call.name == "csv",
                columns: columns
                    .iter()
                    .filter_map(|a| match a {
                        ast::Arg::Lit(Value::Str(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                delimiter,
            });
            // The rows, with a node standing for the record of the
            // columns asked of it, exactly as the command would read
            // them.
            let columns: Vec<ast::Arg> = columns.to_vec();
            call.name = stdlib::ROWS_FOR_RENDERING.to_string();
            call.args = columns;
        }
        self
    }

    /// For a host that draws the table itself (the HTML grid of the
    /// browser hosts): when the query closes with `@| table`, the
    /// closing command is taken off entirely, its shape kept in
    /// [`Outcome::closing`], and the run yields what stands before
    /// it — a node set when the query selected nodes (a document's
    /// own table, say), values when it projected them. Unlike
    /// [`for_rendering`](Self::for_rendering), no stand-in command
    /// turns the nodes into their names; [`table_rows_of`] reads
    /// them into rows. A query closing with `@| csv`, or with any
    /// other command, runs as it stands.
    pub fn without_closing(mut self) -> Query {
        if let Some(ast::Command::Agg(call)) = self.ast.pipeline.last()
            && matches!(call.name.as_str(), "table" | "pretty")
        {
            let (delimiter, columns) = stdlib::split_csv_delimiter(call);
            self.closing = Some(ClosingTable {
                csv: false,
                columns: columns
                    .iter()
                    .filter_map(|a| match a {
                        ast::Arg::Lit(Value::Str(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                delimiter,
            });
            self.ast.pipeline.pop();
        }
        self
    }

    /// Evaluate against `adapter` under `run`.
    ///
    /// Errors: a refusal (a feature the engine does not implement, a
    /// shell command with the gate closed, an argument of the wrong
    /// kind); a malformed query is refused at [`Query::parse`].
    pub fn run(&self, adapter: &impl AstAdapter, run: &Run) -> Result<Outcome> {
        pipeline::gate(&self.ast, &run.options())?;
        let plan = self.planned(adapter, run)?;
        let result = checked(graph::host::eval_with(
            &self.ast,
            adapter,
            Some(run.clone()),
        ))?;
        Ok(Outcome {
            result,
            closing: self.closing.clone(),
            refs: pipeline::take_refs(),
            plan,
        })
    }

    /// The static plan when the run asks for it, held to the run's
    /// budget: a query the estimate puts beyond the budget is
    /// refused here, before anything runs.
    /// Run at the calculator: no arbor, the run's topic on the head,
    /// every thread standing nowhere. The result is always values.
    pub fn run_calc(&self, run: &Run) -> Result<Outcome> {
        pipeline::gate(&self.ast, &run.options())?;
        let values = checked(graph::host::eval_calc(&self.ast, Some(run.clone())))?;
        Ok(Outcome {
            result: QueryResult::Values(values),
            closing: self.closing.clone(),
            refs: pipeline::take_refs(),
            plan: None,
        })
    }

    /// The calculator's traced run: every value with its provenance.
    pub fn run_traced_prov_calc(&self, run: &Run) -> Result<Vec<Traced>> {
        pipeline::gate(&self.ast, &run.options())?;
        checked(graph::host::eval_calc_traced(&self.ast, Some(run.clone())))
    }

    fn planned(&self, adapter: &impl AstAdapter, run: &Run) -> Result<Option<plan::Plan>> {
        if !run.plan && run.budget.is_none() {
            return Ok(None);
        }
        let plan = match &run.budget {
            Some(budget) => plan::of_within(&self.ast, adapter, budget.cap()),
            None => plan::of(&self.ast, adapter),
        };
        if let Some(budget) = &run.budget
            && let Some(why) = budget.exceeded_by(&plan)
        {
            return Err(QuarbError::refused(why));
        }
        Ok(Some(plan))
    }

    /// Like [`Query::run`], returning the final capsae as `(node,
    /// topic)` pairs, so each result value still knows which node
    /// produced it — the provenance-bearing form used by typed
    /// emission (`qua --daiv`). A `None` topic is a node result.
    pub fn run_traced(
        &self,
        adapter: &impl AstAdapter,
        run: &Run,
    ) -> Result<Vec<(NodeId, Option<Value>)>> {
        Ok(self
            .run_traced_prov(adapter, run)?
            .into_iter()
            .map(|t| (t.node, t.topic))
            .collect())
    }

    /// Like [`Query::run_traced`], with each value's provenance: the
    /// origins of its topic, per field for a record
    /// ([`graph::host::Traced`]).
    pub fn run_traced_prov(&self, adapter: &impl AstAdapter, run: &Run) -> Result<Vec<Traced>> {
        pipeline::gate(&self.ast, &run.options())?;
        checked(graph::host::eval_traced_prov_with(
            &self.ast,
            adapter,
            Some(run.clone()),
        ))
    }

    /// The static plan: what the query will touch, step by step,
    /// with what `adapter` says it would apply from its catalog —
    /// no node is read ([`plan::of`]).
    pub fn plan(&self, adapter: &dyn AstAdapter) -> plan::Plan {
        plan::of(&self.ast, adapter)
    }

    /// The static plan priced under `budget`: every estimator may
    /// stop at the first entry past it ([`Cap`]), so a listing of
    /// a hundred thousand files costs a thousand reads under
    /// `rows=1000`.
    pub fn plan_within(&self, adapter: &dyn AstAdapter, budget: &plan::Budget) -> plan::Plan {
        plan::of_within(&self.ast, adapter, budget.cap())
    }

    /// The query rendered back to canonical text (defs expanded).
    pub fn canonical(&self) -> String {
        unparse::unparse(&self.ast)
    }

    /// The syntax tree.
    pub fn ast(&self) -> &ast::Query {
        &self.ast
    }
}

/// Lex, parse, and evaluate `query` against `adapter` under the
/// ambient options ([`Run::ambient`]; inside a run, that run's).
///
/// Returns the [`QueryResult`] — a node set, or scalar values if the
/// query ends in a projection — or an error if the query is malformed
/// or uses a feature the engine does not implement yet. A tool
/// choosing its options runs a [`Query`] under a [`Run`].
pub fn run(query: &str, adapter: &impl AstAdapter) -> Result<QueryResult> {
    run_with_defs(query, &Defs::default(), adapter)
}

/// Like [`run`], for a tool that renders under an output locale
/// ([`Query::for_rendering`]).
pub fn run_for_rendering(
    query: &str,
    adapter: &impl AstAdapter,
) -> Result<(QueryResult, Option<ClosingTable>)> {
    let r = inherited();
    let out = Query::parse_with(query, &Defs::default(), adapter, &r)?
        .for_rendering()
        .run(adapter, &r)?;
    Ok((out.result, out.closing))
}

/// Like [`run`], returning `(node, topic)` pairs
/// ([`Query::run_traced`]).
pub fn run_traced(query: &str, adapter: &impl AstAdapter) -> Result<Vec<(NodeId, Option<Value>)>> {
    run_traced_with_defs(query, &Defs::default(), adapter)
}

/// Like [`run_traced`], with each value's provenance
/// ([`Query::run_traced_prov`]).
pub fn run_traced_prov(query: &str, adapter: &impl AstAdapter) -> Result<Vec<graph::host::Traced>> {
    run_traced_prov_with_defs(query, &Defs::default(), adapter)
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

/// Whether `text` is definitions and nothing else (`#` comments
/// allowed): `Ok(false)` when a query follows the definitions, an
/// error when a definition itself is malformed.
pub fn defs_only(text: &str) -> Result<bool> {
    let tokens = lexer::lex(&strip_defs_comments(text))?;
    parser::defs_only(&tokens)
}

/// Blank out the `#` comment lines of a defs file, preserving line
/// structure. Session layers that *prepend defs text to query text*
/// (quai, the notebook kernels) must store the stripped form — the
/// query lexer itself has no comment syntax.
pub fn strip_defs_comments(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.trim_start().starts_with('#') {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Like [`run`], with a pre-seeded fragment table (a `--defs` file);
/// inline `def` statements extend it.
pub fn run_with_defs(query: &str, defs: &Defs, adapter: &impl AstAdapter) -> Result<QueryResult> {
    let r = inherited();
    Ok(Query::parse_with(query, defs, adapter, &r)?
        .run(adapter, &r)?
        .result)
}

/// Like [`run_traced`], with a pre-seeded fragment table.
pub fn run_traced_with_defs(
    query: &str,
    defs: &Defs,
    adapter: &impl AstAdapter,
) -> Result<Vec<(NodeId, Option<Value>)>> {
    let r = inherited();
    Query::parse_with(query, defs, adapter, &r)?.run_traced(adapter, &r)
}

/// Like [`run_traced_prov`], with a pre-seeded fragment table.
pub fn run_traced_prov_with_defs(
    query: &str,
    defs: &Defs,
    adapter: &impl AstAdapter,
) -> Result<Vec<graph::host::Traced>> {
    let r = inherited();
    Query::parse_with(query, defs, adapter, &r)?.run_traced_prov(adapter, &r)
}

/// True if `query` (inline defs allowed) opens with an expression
/// head or a lone root anchor and never navigates a document: every
/// branch is a step-less, projection-less root anchor and there are
/// no correlations. Such a query can run against a bare root with
/// no mounted target — the calculator invocation (spec: The
/// Expression Head). Unparseable text answers `false`; the caller's
/// normal run path owns the error message.
/// The first operand of a calculator query that reads a document —
/// a path from the root or a mark (`= (/rows/*::x)`), which a bare
/// root answers with nothing — in its written form, or `None` when
/// the query computes from its literals alone. The calculator
/// command `qc` refuses such a query by name, pointing at `qua`.
pub fn calculator_reads(query: &str) -> Option<String> {
    let tokens = lexer::lex(query).ok()?;
    let ast = parser::parse_with_defs(&tokens, Defs::default()).ok()?;
    struct Reads(Option<String>);
    impl ast::visit::Visitor for Reads {
        fn operand(&mut self, o: &ast::Operand) {
            if self.0.is_none()
                && let ast::Operand::Rel { steps, .. } = o
                && !steps.is_empty()
            {
                self.0 = Some(unparse::operand(o));
            }
            ast::visit::walk_operand(self, o);
        }
    }
    let mut v = Reads(None);
    ast::visit::walk_query(&mut v, &ast);
    v.0
}

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
    parser::parse_first_steps(&tokens, defs.clone(), &graph::reflect::Reflector::new(None))
}

/// Like [`expand_first`], with the dataset at hand, so data-aware
/// macros (`&name!`) can read it at expansion time, under `run`
/// (their shell gate, their instant).
pub fn expand_first_with(
    query: &str,
    defs: &Defs,
    adapter: &impl AstAdapter,
    run: &Run,
) -> Result<Vec<String>> {
    let _scope = graph::run::enter(Some(run.clone()));
    let tokens = lexer::lex(query)?;
    parser::parse_first_steps(
        &tokens,
        defs.clone(),
        &graph::reflect::Reflector::new(Some(adapter)),
    )
}

/// Like [`expand`], with the dataset at hand, so data-aware macros
/// (`&name!`) can read it at expansion time, under `run`.
pub fn expand_with(
    query: &str,
    defs: &Defs,
    adapter: &impl AstAdapter,
    run: &Run,
) -> Result<String> {
    Ok(Query::parse_with(query, defs, adapter, run)?.canonical())
}
