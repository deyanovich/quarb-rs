//! Parser for the supported query subset.
//!
//! Grammar (sketch):
//!
//! ```text
//! parse   := query ('<=>' query)*         -- correlation chain
//! query   := branch ('||' branch)* command*
//! branch  := '^'? step* projection?
//! step    := axis matcher trait* pred* '$'?
//! command   := '|' (func | push | subcontext | recall) | '@|' func
//! pred    := '[' n ']' | '[' or_expr ']'
//! ```
//!
//! A recursive-descent parser; `parse_query` also nests inside a
//! subcontext body `.(…)`.

use crate::adapter::AstAdapter;
use crate::ast::visit::{self, Visitor as _};
use crate::ast::{
    Anchor, Arg, ArithOp, Axis, Branch, CmpOp, Command, FnCall, Group, InterpSeg, Matcher, Operand,
    PatSeg, PathElem, PredExpr, Predicate, Projection, PushBody, Quant, Query, Reach, RefProp,
    RegRef, ScopePath, Step, TraitClause,
};
use crate::error::{QuarbError, Result};
use crate::lexer::{self, Token};
use crate::value::Value;
use globset::Glob;
use regex::Regex;
use std::collections::HashMap;

/// Parse a token stream into a [`Query`], expanding any inline
/// `def` statements.
/// Parse a scope path on its own (`/orders/*`, `//a[::x]`) into the
/// shape the arrow's argument carries, so a declaration naming a
/// scope compares with a query's by canonical text.
pub fn parse_scope_path(text: &str) -> Result<ScopePath> {
    let toks = crate::lexer::lex(text)?;
    let mut p = Parser::new(&toks, Defs::default(), None);
    let sp = p.scope_path()?;
    if p.pos < toks.len() {
        return Err(QuarbError::parse(format!(
            "a scope path ends with its last step: '{text}'"
        )));
    }
    sp.ok_or_else(|| QuarbError::parse(format!("not a scope path: '{text}'")))
}

pub fn parse(tokens: &[Token]) -> Result<Query> {
    parse_with_defs(tokens, Defs::default())
}

/// Parse a token stream into a [`Query`] with a pre-seeded fragment
/// table (`--defs`); inline `def` statements extend it.
pub fn parse_with_defs(tokens: &[Token], defs: Defs) -> Result<Query> {
    parse_with_data(tokens, defs, None)
}

/// Parse with a fragment table and the dataset being queried; the
/// dataset is what a data-aware macro (`&name!`) reads at expansion
/// time, mounted as `/data` in its expansion arbor.
pub fn parse_with_data(
    tokens: &[Token],
    defs: Defs,
    data: Option<&dyn AstAdapter>,
) -> Result<Query> {
    parse_with_data_spanned(tokens, None, defs, data)
}

/// [`parse_with_data`] with the tokens' spans (character offsets
/// from the lexer, `lex_spanned`): a parse error then says where
/// in the query it stopped.
pub fn parse_with_data_spanned(
    tokens: &[Token],
    spans: Option<&[(usize, usize)]>,
    defs: Defs,
    data: Option<&dyn AstAdapter>,
) -> Result<Query> {
    let mut p = Parser::new(tokens, defs, data);
    let out = p.parse();
    match (out, spans) {
        (Err(e), Some(spans)) if e.kind() == crate::error::Kind::Parse && e.offset().is_none() => {
            let at = p.pos.min(spans.len().saturating_sub(1));
            match spans.get(at) {
                Some(&(start, _)) => Err(e.at(start)),
                None => Err(e),
            }
        }
        (out, _) => out,
    }
}

/// Parse fully (so every diagnostic still fires), returning each
/// DIRECTLY-invoked macro's generated text before its re-expansion
/// — the `macroexpand-1` lens (`qua --expand-1`). One entry per
/// direct invocation, in source order; invocations nested inside
/// generated text expand in nested parsers and do not report here
/// (run again on the printed text to take the next step).
pub fn parse_first_steps(
    tokens: &[Token],
    defs: Defs,
    data: Option<&dyn AstAdapter>,
) -> Result<Vec<String>> {
    let mut p = Parser::new(tokens, defs, data);
    p.parse()?;
    Ok(p.first_steps.into_inner())
}

/// Parse a token stream containing only `def` statements into a
/// fragment table (the `--defs` file format).
pub fn parse_defs(tokens: &[Token]) -> Result<Defs> {
    let mut p = Parser::new(tokens, Defs::default(), None);
    while p.at_def() || p.at_macro() {
        if p.at_def() {
            p.parse_def()?;
        } else {
            p.parse_macro()?;
        }
    }
    if let Some(tok) = p.peek() {
        return Err(QuarbError::parse(format!(
            "a definitions file holds only 'def' and 'macro' statements; unexpected {tok:?}"
        )));
    }
    Ok(p.defs)
}

/// A table of named fragments (`def &name: body;`), expanded at
/// parse time. Names are unique; a definition may invoke only
/// *earlier* definitions, so recursion is impossible by
/// construction.
#[derive(Debug, Clone, Default)]
pub struct Defs {
    entries: Vec<(String, Def)>,
}

impl Defs {
    fn get(&self, name: &str) -> Option<&Def> {
        self.entries.iter().find(|(n, _)| n == name).map(|(_, d)| d)
    }

    /// The table as it stood before `name` was defined. Macro
    /// expansion text is reparsed against this, so a macro may
    /// invoke only *earlier* fragments — recursion stays impossible
    /// even through generated text.
    fn before(&self, name: &str) -> Defs {
        let end = self
            .entries
            .iter()
            .position(|(n, _)| n == name)
            .unwrap_or(self.entries.len());
        Defs {
            entries: self.entries[..end].to_vec(),
        }
    }
}

/// One fragment: parameter names and a body. `rest` is a trailing
/// `@name` rest-parameter (macros only) collecting the remaining
/// invocation arguments.
#[derive(Debug, Clone)]
struct Def {
    params: Vec<String>,
    rest: Option<String>,
    /// Data-aware (`macro &name!`): the body reads the dataset,
    /// mounted as `/data`, and every invocation spells the `!`.
    data_aware: bool,
    body: DefBody,
}

/// A fragment body: a navigation query (which may carry its own
/// pipeline), a pipeline fragment (each command's pipe is implied by
/// its variant), a predicate fragment (`def &vis: [cond];` — a
/// guard, spliced onto the step before it or read as a boolean
/// operand), or a procedural macro — a query evaluated at expansion
/// time against the invocation's expansion arbor, whose text
/// results become the expansion.
#[derive(Debug, Clone)]
enum DefBody {
    Query(Query),
    Pipeline(Vec<Command>),
    Predicates(Vec<Predicate>),
    Macro(Query),
}

/// Which pipe introduces a command — determined by its variant.
fn command_pipe(command: &Command) -> &'static str {
    match command {
        Command::Agg(_)
        | Command::Select(_)
        | Command::AggPush { .. }
        | Command::AggRecord { .. }
        | Command::AggRecall(_) => "@|",
        _ => "|",
    }
}

/// One macro expansion's contribution to a register name, for the
/// ruling-#22 sweep: what the generated text itself pushed and
/// recalled. Recorded only for names no argument invited.
struct CaptureRec {
    mac: String,
    reg: String,
    pushes: usize,
    recalls: usize,
}

/// The union of register names a macro's arguments invite (ruling
/// #22) — computed before the arguments are consumed by expansion.
fn invites_of(args: &[Operand]) -> std::collections::BTreeSet<String> {
    let mut s = std::collections::BTreeSet::new();
    for a in args {
        s.extend(crate::reflect::operand_invites(a));
    }
    s
}

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    /// Fragments defined so far (pre-seeded plus inline).
    defs: Defs,
    /// The parameter names in scope while parsing a def body
    /// (empty outside one); `$name` operands must be among them.
    def_params: Vec<String>,
    /// The dataset being queried, when the caller has one — what a
    /// data-aware macro (`&name!`) mounts as `/data` at expansion.
    data: Option<&'a dyn AstAdapter>,
    /// How many path-pattern groups enclose the current position.
    /// Inside one, a bare `.` in matcher position is the pattern dot
    /// wildcard; outside, it stays a literal name. Predicates reset
    /// the scope (their operand paths are not pattern content).
    pattern_depth: usize,
    /// How many predicates enclose the current position. Inside one,
    /// reverse resolution (`<--`) is refused: it walks the whole arbor
    /// per candidate node, so the spec restricts a predicate's nested
    /// paths to descending navigation and outgoing edges (`<-` — an
    /// adapter-indexed backlink — is allowed).
    predicate_depth: usize,
    /// How deeply the recursive-descent constructs (parenthesized
    /// expressions, path groups, `!` chains) are nested right now.
    /// Bounded by [`MAX_NEST`]: unbounded, a long run of `(` is a
    /// stack overflow (an abort, not an error) — and macro expansion
    /// re-enters the parser, so adversarial *data* can reach this.
    nest_depth: usize,
    /// How many try-a-query fallback sites enclose the current
    /// position (subcontext bodies, pattern pushes). Inside one, the
    /// expression head is off: those sites probe `parse_query` and
    /// fall back to a value expression, and the head would make the
    /// probe spuriously succeed with a root anchor where the value
    /// reading (anchored at the current node) is the meaning.
    subquery_depth: usize,
    /// How many command parentheses (operand lists) enclose the current
    /// position. A command's parentheses hold its operands and options, and a
    /// bare word there is the short spelling of a quoted string
    /// (`stem(ru)`, `sort(atergo)`); everywhere else — a body —
    /// a bare word names a command applied to the body's seed.
    arg_depth: usize,
    /// Ruling #22 (invited capture): the uninvited register pushes
    /// of each macro expansion this parser directly initiated, held
    /// for the end-of-parse sweep. `RefCell` because expansion runs
    /// behind `&self`.
    captures: std::cell::RefCell<Vec<CaptureRec>>,
    /// The generated text of each macro expansion this parser
    /// directly initiated, in source order — `macroexpand-1`'s raw
    /// material (`qua --expand-1`). Nested invocations expand in
    /// nested parsers and do not report here.
    first_steps: std::cell::RefCell<Vec<String>>,
}

/// Nesting depth past which parsing refuses (see
/// [`Parser::nest_depth`]). Far beyond any real query, and small
/// enough that the deepest frame chain (each `(` costs several
/// stack frames through the group/operand dual reading) fits the
/// smallest stacks in play — test threads (2 MB) and wasm.
const MAX_NEST: usize = 64;

/// Where a path-position splice stands (spec: Bodies and Splice
/// Positions) — inside a branch's walk, or as (part of) a group
/// alternative. The distinction matters only for projections: a
/// projected body may end a branch, but a group alternative walks on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SplicePos {
    MidPath,
    GroupAlt,
}

/// Whether a fragment body (or a macro expansion) is plain
/// navigation the branch machinery can splice into a walk: no
/// pipeline, no correlation, every branch starting at the current
/// node — and, for a union, no projections (a multi-branch splice
/// becomes a path-pattern group, and groups carry no projections).
fn splices_as_path(q: &Query) -> bool {
    q.correlations.is_empty()
        && q.pipeline.is_empty()
        && q.branches.iter().all(|b| b.anchor == Anchor::Current)
        && (q.branches.len() == 1 || q.branches.iter().all(|b| b.projection.is_none()))
}

/// Wrap spliced elements in a path-pattern group carrying the
/// trailing refinement. A body that is already exactly one bare
/// group (the `def &clean: (…);` idiom) adopts the refinement
/// directly, so the canonical form stays flat: `/cookies/C&clean+`
/// prints as `/cookies/C(…)+`, not `((…))+`.
fn group_wrap(
    elems: Vec<PathElem>,
    quant: Quant,
    predicates: Vec<Predicate>,
    reach: Reach,
) -> Vec<PathElem> {
    if elems.len() == 1
        && let Some(PathElem::Group(g)) = elems.first()
        && g.quant
            == (Quant {
                min: 1,
                max: Some(1),
            })
        && g.predicates.is_empty()
        && g.reach == Reach::All
    {
        let Some(PathElem::Group(mut g)) = elems.into_iter().next() else {
            unreachable!("shape checked above");
        };
        g.quant = quant;
        g.predicates = predicates;
        g.reach = reach;
        return vec![PathElem::Group(g)];
    }
    vec![PathElem::Group(Group {
        alts: vec![elems],
        quant,
        predicates,
        traits: Vec::new(),
        reach,
    })]
}

/// Whether any step in these elements walks reverse resolution
/// (`<--`) — refused inside predicates, where a def body parsed at
/// depth zero could otherwise smuggle it in. Push bodies parse in
/// their own scopes and stay out of the walk.
fn walks_reverse_resolve(elems: &[PathElem]) -> bool {
    elems.iter().any(|e| match e {
        PathElem::Step(s) => matches!(
            s.axis,
            Axis::ReverseResolve { scope: None, .. } | Axis::ValueMatch { scope: None, .. }
        ),
        PathElem::Group(g) => g.alts.iter().any(|a| walks_reverse_resolve(a)),
        PathElem::Mark(_) | PathElem::Push { .. } => false,
    })
}

/// The static execution mode between pipeline commands (spec:
/// Execution Modes). Navigation mode: the thread is a node with no
/// live topic — hops may fan out. Scalar mode: a projection or
/// command has produced a live topic — a hop would drop it, so
/// path-shaped commands refuse until a push (`| .`) files the topic
/// and returns the thread to navigation mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PipeMode {
    Nav,
    Scalar,
}

/// A command's mode-out given its mode-in. Pushes of every spelling
/// return to navigation mode (the push files the topic; spec:
/// "Pushes transition from scalar mode back to navigation mode").
/// Filters and positional selection preserve the mode; everything
/// that produces or transforms a topic enters scalar mode; a
/// navigation command ends in the mode its optional projection
/// implies.
fn command_mode_out(command: &Command, cur: PipeMode) -> PipeMode {
    match command {
        Command::Nav(b) => {
            if b.projection.is_some() {
                PipeMode::Scalar
            } else {
                PipeMode::Nav
            }
        }
        Command::Push(_)
        | Command::ExprPush { .. }
        | Command::RecordPush { .. }
        | Command::FieldsPush
        | Command::Subcontext { .. } => PipeMode::Nav,
        Command::Filter(_) | Command::Select(_) | Command::AggPush { .. } => cur,
        Command::Func(_)
        | Command::Expr(_)
        | Command::Agg(_)
        | Command::AggRecord { .. }
        | Command::AggRecall(_)
        | Command::Recall(_)
        | Command::RecordWith(_)
        | Command::Spread { .. }
        | Command::Map(_) => PipeMode::Scalar,
        Command::Repeat { command, .. } => command_mode_out(command, cur),
    }
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    /// Enter one level of recursive nesting; a parse error past the
    /// bound, instead of a stack overflow. Callers pair it with a
    /// decrement after the recursive call returns.
    fn descend(&mut self) -> Result<()> {
        self.nest_depth += 1;
        if self.nest_depth > MAX_NEST {
            return Err(QuarbError::parse(format!(
                "query nested more than {MAX_NEST} levels deep"
            )));
        }
        Ok(())
    }

    /// A parser at the top of a query: fresh scopes, depth zero.
    fn new(toks: &'a [Token], defs: Defs, data: Option<&'a dyn AstAdapter>) -> Parser<'a> {
        Parser {
            toks,
            pos: 0,
            defs,
            def_params: Vec::new(),
            data,
            pattern_depth: 0,
            predicate_depth: 0,
            nest_depth: 0,
            subquery_depth: 0,
            arg_depth: 0,
            captures: std::cell::RefCell::new(Vec::new()),
            first_steps: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// A parser for text this one generated or re-reads (a macro
    /// expansion, a substitution body, a def body): one level
    /// deeper (refused past [`MAX_NEST`]), the dataset shared, the
    /// scopes reset — a caller that inherits a scope sets the field
    /// after. `defs` is the table the child sees (`self.defs.before`
    /// for an expansion, a clone for a re-read).
    fn child<'t>(&self, toks: &'t [Token], defs: Defs) -> Result<Parser<'t>>
    where
        'a: 't,
    {
        Ok(Parser {
            nest_depth: self.sub_depth()?,
            ..Parser::new(toks, defs, self.data)
        })
    }
}

impl Parser<'_> {
    /// The depth a nested parser starts at — one level below this
    /// one, so a hole inside a hole inside a hole (`${"${…}"}`),
    /// a macro expanding inside its own expansion, or generated
    /// text nesting further all count against the same bound
    /// instead of starting afresh and overflowing the stack.
    fn sub_depth(&self) -> Result<usize> {
        let depth = self.nest_depth + 1;
        if depth > MAX_NEST {
            return Err(QuarbError::parse(format!(
                "query nested more than {MAX_NEST} levels deep"
            )));
        }
        Ok(depth)
    }

    fn bump(&mut self) -> Option<&Token> {
        let t = self.toks.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse(&mut self) -> Result<Query> {
        if self.toks.is_empty() {
            return Err(QuarbError::parse("empty query"));
        }
        // Inline definitions precede the query.
        while self.at_def() || self.at_macro() {
            if self.at_def() {
                self.parse_def()?;
            } else {
                self.parse_macro()?;
            }
        }
        let query = self.parse_chain()?;
        if let Some(tok) = self.peek() {
            // `:name` after a path: a node has properties, not
            // fields.
            if matches!(tok, Token::Field)
                && let Some(Token::Name { text, .. }) = self.toks.get(self.pos + 1)
            {
                return Err(QuarbError::parse(format!(
                    "':{text}' reads a record's field; a node's property is '::{text}'"
                )));
            }
            return Err(QuarbError::parse(format!(
                "unexpected trailing input at token {tok:?}"
            )));
        }
        validate_correlation_refs(&query)?;
        validate_keyed_commands(&query)?;
        self.sweep_captures(&query)?;
        Ok(query)
    }

    /// A chain of expressions joined by `<=>`. The FIRST expression
    /// drives (SQL's FROM); each subsequent one is a joined
    /// expression whose correlated predicates — the ON clause —
    /// reference the driver as `$$…` and earlier joined expressions
    /// as `$*k`. Shared by top-level queries and `def` bodies (a
    /// fragment may name a whole join).
    fn parse_chain(&mut self) -> Result<Query> {
        let mut query = self.parse_query()?;
        // The join binds where it is written: after the driver's
        // commands parsed so far (a spliced chain-carrying fragment
        // brought its own position along).
        if matches!(self.peek(), Some(Token::Correlate)) && query.correlations.is_empty() {
            query.join_at = query.pipeline.len();
        }
        while matches!(self.peek(), Some(Token::Correlate)) {
            self.pos += 1;
            // `<=>?` — the outer marker flags the expression it
            // precedes: that context may bind null (LEFT JOIN).
            let outer = if matches!(self.peek(), Some(Token::Question { .. })) {
                self.pos += 1;
                true
            } else {
                false
            };
            let mut entry = self.parse_query()?;
            if !entry.correlations.is_empty() {
                return Err(QuarbError::parse(
                    "a joined expression cannot itself carry a \
                     correlation — chains are flat; splice a \
                     chain-carrying fragment at driver position",
                ));
            }
            entry.outer = outer;
            query.correlations.push(entry);
        }
        // The pipeline written after the last joined expression is
        // the driver's continuation (`A <=> B[on] | rec(...)` recs
        // the driving thread); a pipeline on an earlier entry stays
        // that entry's own pre-join shaping.
        if let Some(last) = query.correlations.last_mut() {
            query.pipeline.append(&mut last.pipeline);
        }
        Ok(query)
    }

    /// Parse a query — a union of branches followed by a pipeline —
    /// stopping at a `)`, a `;`, or the end of input (so it nests
    /// inside a subcontext or a def body).
    fn parse_query(&mut self) -> Result<Query> {
        // Union of branches (binds tighter than the pipeline). An
        // element may be a fragment invocation, whose branches (and,
        // if it stands alone, pipeline) splice in.
        let mut branches = Vec::new();
        let mut pipeline = Vec::new();
        let mut correlations = Vec::new();
        let mut join_at = 0usize;
        self.union_element(
            &mut branches,
            &mut pipeline,
            &mut correlations,
            &mut join_at,
        )?;
        // `||` or its word (`.or.`, the family's spellings): the
        // rounding follows the glyph, not the position.
        while matches!(self.peek(), Some(Token::PipePipe)) || self.peek_word(OR_WORDS) {
            if !pipeline.is_empty() || !correlations.is_empty() {
                return Err(QuarbError::parse(
                    "a fragment carrying a pipeline or a correlation must \
                     stand alone, not in a union",
                ));
            }
            self.pos += 1;
            self.union_element(
                &mut branches,
                &mut pipeline,
                &mut correlations,
                &mut join_at,
            )?;
        }

        // Pipeline over the whole union. The entering mode is
        // static: a projected branch enters in scalar mode, plain
        // navigation in navigation mode; commands spliced from a
        // fragment have already advanced it.
        let mut mode = if branches.iter().any(|b| b.projection.is_some()) {
            PipeMode::Scalar
        } else {
            PipeMode::Nav
        };
        for s in &pipeline {
            mode = command_mode_out(s, mode);
        }
        self.pipeline_items(&mut pipeline, mode)?;
        Ok(Query {
            correlations,
            outer: false,
            branches,
            pipeline,
            join_at,
        })
    }

    /// One element of a branch union: a plain branch, or a
    /// query-fragment invocation spliced in.
    fn union_element(
        &mut self,
        branches: &mut Vec<Branch>,
        pipeline: &mut Vec<Command>,
        correlations: &mut Vec<Query>,
        join_at: &mut usize,
    ) -> Result<()> {
        // The expression head: a query may open with `= expr`
        // instead of navigation — `= 2 + 2`, `= 1900-01-01 |
        // &gregorian`. Sugar for `^ | (expr)`: one row from the
        // root anchor, the expression's value as its topic (the
        // calculator entry — no document navigation required). The
        // sigil is collision-free: no query form starts with `=`
        // (equality is infix, inside predicates), so parentheses
        // keep every navigational meaning they had. Off inside
        // try-a-query fallback sites (subquery_depth): a subcontext
        // body's value reading, anchored at the current node, is
        // the meaning there.
        if branches.is_empty() && self.subquery_depth == 0 && matches!(self.peek(), Some(Token::Eq))
        {
            self.pos += 1;
            let expr = self.additive()?;
            branches.push(Branch {
                steps: Vec::new(),
                projection: None,
                anchor: Anchor::Root,
            });
            pipeline.push(Command::Expr(expr));
            if matches!(self.peek(), Some(Token::PipePipe)) || self.peek_word(OR_WORDS) {
                return Err(QuarbError::parse(
                    "an expression head stands alone; it cannot join a \
                     union — pipe from it instead: '= expr | …'",
                ));
            }
            return Ok(());
        }
        if matches!(self.peek(), Some(Token::Amp)) {
            let alone = branches.is_empty();
            // A predicate fragment refines a preceding element;
            // route it through the branch machinery, which reports
            // that nothing precedes it here.
            if matches!(self.peek_invocation_body(), Some(DefBody::Predicates(_))) {
                branches.push(self.branch()?);
                return Ok(());
            }
            let (name, q, quant) = self.invoke_query_fragment()?;
            if quant.is_some() && !splices_as_path(&q) {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' carries a pipeline, a correlation, \
                     or an anchor; a quantifier cannot ride it — quantify \
                     plain navigation"
                )));
            }
            // A plain-navigation body that is refined or continued
            // goes through the branch machinery, where mid-path
            // splicing lives (`&card/h3::`, `&clean+`, `&m[p]`). A
            // bare one splices whole — the historical head
            // behavior, `&either` ≡ `/a || /b` — as do bodies
            // carrying a pipeline, a correlation, an anchor, or a
            // projected union.
            if splices_as_path(&q)
                && (q.branches.len() == 1 || quant.is_some() || self.splice_refinement_ahead())
            {
                let (elems, projection) =
                    self.finish_splice(&name, q.branches, quant, SplicePos::MidPath)?;
                branches.push(self.branch_tail(Anchor::Current, elems, projection)?);
                return Ok(());
            }
            if (!q.pipeline.is_empty() || !q.correlations.is_empty()) && !alone {
                return Err(QuarbError::parse(
                    "a fragment carrying a pipeline or a correlation must \
                     stand alone, not in a union",
                ));
            }
            branches.extend(q.branches);
            if !q.correlations.is_empty() {
                *join_at = pipeline.len() + q.join_at;
            }
            pipeline.extend(q.pipeline);
            correlations.extend(q.correlations);
            if matches!(self.peek(), Some(Token::LBracket)) {
                return Err(QuarbError::parse(
                    "a fragment carrying a pipeline, a correlation, or an \
                     anchor does not take trailing predicates; refine it \
                     with a pipeline filter: '&name | [cond]'",
                ));
            }
            if matches!(
                self.peek(),
                Some(Token::ColonColon | Token::ColonColonColon | Token::SemiSemiSemi)
            ) {
                return Err(QuarbError::parse(
                    "a fragment carrying a pipeline, a correlation, or an \
                     anchor does not take a trailing projection; project \
                     it through the pipe: '&name | ::key'",
                ));
            }
            if matches!(self.peek(), Some(Token::Lt { .. })) {
                return Err(QuarbError::parse(
                    "a fragment does not take a trailing trait selector; \
                     put the trait inside a definition: \
                     'def &errs: /entry<error> ;'",
                ));
            }
        } else {
            branches.push(self.branch()?);
        }
        Ok(())
    }

    /// The defined body of the invocation ahead (`&name…`), for
    /// splice-site routing; `None` when the name is unknown (the
    /// invocation path itself reports that).
    fn peek_invocation_body(&self) -> Option<&DefBody> {
        match self.toks.get(self.pos + 1) {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => self.defs.get(text).map(|d| &d.body),
            _ => None,
        }
    }

    /// Whether the tokens after a head-position invocation refine or
    /// continue it — a quantifier, predicates, a projection, another
    /// hop or invocation — which routes the splice through the
    /// branch machinery instead of the historical whole splice.
    fn splice_refinement_ahead(&self) -> bool {
        match self.peek() {
            Some(
                Token::Quant { .. }
                | Token::LBracket
                | Token::Amp
                | Token::Slash
                | Token::SlashSlash
                | Token::Backslash
                | Token::BackslashBackslash
                | Token::ArrowOut
                | Token::ArrowIn
                | Token::DashDash
                | Token::LParen
                | Token::ColonColon
                | Token::ColonColonColon
                | Token::SemiSemiSemi
                | Token::Lt { .. }
                | Token::Gt { .. }
                | Token::NextSibling
                | Token::PrevSibling,
            ) => true,
            Some(Token::Name {
                text,
                quoted: false,
                glued: true,
            }) => text == "+" || text == "*",
            _ => false,
        }
    }

    /// Parse pipeline commands onto `pipeline` until something that is
    /// not a pipe. Shared by queries and pipeline-fragment bodies.
    /// `mode` is the static execution mode entering the first command
    /// (spec: Execution Modes) — it decides whether a path-shaped
    /// command is navigation or an error, and each command's mode-out
    /// feeds the next.
    fn pipeline_items(&mut self, pipeline: &mut Vec<Command>, mut mode: PipeMode) -> Result<()> {
        loop {
            match self.peek() {
                Some(Token::Pipe) => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(Token::Amp)) {
                        let before = pipeline.len();
                        self.invoke_pipeline_fragment("|", pipeline)?;
                        for s in &pipeline[before..] {
                            mode = command_mode_out(s, mode);
                        }
                        continue;
                    }
                    let mut command = self.pipe_item(mode)?;
                    // `.v(body){N}` — a quantifier on a push repeats
                    // it.
                    if let Some(&Token::Quant { min, max }) = self.peek() {
                        if !matches!(
                            command,
                            Command::Push(_)
                                | Command::ExprPush { .. }
                                | Command::Subcontext { .. }
                                | Command::RecordPush { .. }
                                | Command::FieldsPush
                        ) {
                            return Err(QuarbError::parse(
                                "a quantifier repeats a push: write it after '.name(body)' (or '.', '.%', '.name%(…)')",
                            ));
                        }
                        if max.is_some_and(|n| n < min) {
                            return Err(QuarbError::parse(format!(
                                "a quantifier's bounds are ordered: {{{min};{}}}",
                                max.unwrap()
                            )));
                        }
                        self.pos += 1;
                        command = Command::Repeat {
                            command: Box::new(command),
                            min,
                            max,
                        };
                    }
                    mode = command_mode_out(&command, mode);
                    pipeline.push(command);
                }
                // `$| command` — the map pipe.
                Some(Token::Dollar) if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) => {
                    self.pos += 2;
                    let command = Command::Map(Box::new(self.map_command()?));
                    mode = command_mode_out(&command, mode);
                    pipeline.push(command);
                }
                Some(Token::At) => {
                    self.pos += 1;
                    self.expect(Token::Pipe, "'|' after '@' for aggregation")?;
                    if matches!(self.peek(), Some(Token::Amp)) {
                        let before = pipeline.len();
                        self.invoke_pipeline_fragment("@|", pipeline)?;
                        for s in &pipeline[before..] {
                            mode = command_mode_out(s, mode);
                        }
                        continue;
                    }
                    // Hops are per-thread; `@|` aggregates the
                    // whole context.
                    if self.nav_command_ahead() {
                        return Err(QuarbError::parse(
                            "navigation is per-thread; write '| /path' \
                             ('@|' aggregates the whole context — hops don't take it)",
                        ));
                    }
                    // `@| [n]` / `@| [a..b]` — positional selection
                    // from the whole context.
                    if matches!(self.peek(), Some(Token::LBracket)) {
                        match self.predicate()? {
                            pred @ (Predicate::Index(_) | Predicate::Range(_, _)) => {
                                // Select preserves the mode.
                                pipeline.push(Command::Select(pred));
                                continue;
                            }
                            Predicate::Expr(_) => {
                                return Err(QuarbError::parse(
                                    "a condition filters per capsa; write '| [cond]' \
                                     ('@| [n]' selects positionally)",
                                ));
                            }
                        }
                    }
                    if let Some(command) = self.agg_sigil_command()? {
                        mode = command_mode_out(&command, mode);
                        pipeline.push(command);
                        continue;
                    }
                    let call = self.func_call()?;
                    let command = agg_command(call)?;
                    mode = command_mode_out(&command, mode);
                    pipeline.push(command);
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Parse one pipeline command after a `|`: a navigation command, a
    /// recall, a push, a subcontext, or a command. `mode` is the
    /// static execution mode entering this command: path-shaped
    /// commands are navigation in navigation mode and an error in
    /// scalar mode (a live topic would be dropped — file it first).
    fn pipe_item(&mut self, mode: PipeMode) -> Result<Command> {
        // A command that opens with a reference arrow — `::p-->`,
        // `::p=>(…)`, the bare `-->`, `<--(…)` — is a hop like
        // `->x`: the command ends on the reached nodes (or on the
        // projection written after them). Only a trailing operator
        // makes it the structural-test expression instead.
        if matches!(mode, PipeMode::Nav)
            && (self.is_resolution_ahead()
                || matches!(self.peek(), Some(Token::Resolve | Token::ReverseResolve)))
        {
            let save = self.pos;
            let b = self.branch()?;
            if !self.expr_continues() {
                return Ok(Command::Nav(b));
            }
            self.pos = save;
            return Ok(Command::Expr(self.additive()?));
        }
        // A command that starts like a path is a navigation command —
        // the pipeline spelling of a path continuation.
        if self.nav_command_ahead() {
            match mode {
                PipeMode::Scalar => {
                    // A parenthesized start keeps its expression
                    // reading (the documented escape); a bare axis
                    // start is a hop, and a hop would drop the
                    // live topic.
                    if !matches!(self.peek(), Some(Token::LParen)) {
                        return Err(QuarbError::parse(
                            "cannot navigate in scalar mode — the topic is live \
                             and a hop would drop it; file it first with '| .' \
                             (or '| .name') and navigation resumes. \
                             (Parenthesize the path for a value expression.)",
                        ));
                    }
                }
                PipeMode::Nav => {
                    // `(` is ambiguous: a quantified-walk group
                    // (`| (->e)+`) or a parenthesized expression
                    // (`| (/kids/*::name)`). Try the branch
                    // reading; fall back to the expression.
                    if matches!(self.peek(), Some(Token::LParen)) && !self.mark_anchor_ahead() {
                        let save = self.pos;
                        if let Ok(b) = self.branch()
                            && !self.expr_continues()
                        {
                            return Ok(Command::Nav(b));
                        }
                        self.pos = save;
                    } else {
                        // A dangling expression operator after the
                        // branch (`| /price:: * 2`) means the command
                        // was arithmetic all along — keep the
                        // operand reading. A bare path is a hop.
                        let save = self.pos;
                        let b = self.branch()?;
                        if !self.expr_continues() {
                            return Ok(Command::Nav(b));
                        }
                        self.pos = save;
                        return Ok(Command::Expr(self.additive()?));
                    }
                }
            }
        }
        match self.peek() {
            // `| $.name`, `| $_`, `| $ord`, and any arithmetic over
            // them are value expressions; a plain recall is just the
            // single-operand case.
            Some(Token::Dollar) => Ok(Command::Expr(self.additive()?)),
            // `| _::prop` / `| _/kid::x` — the served node as the
            // command's expression.
            Some(Token::Driver) => Ok(Command::Expr(self.additive()?)),
            // `| :name` — the topic record's field; `| %+` — the
            // named-captures record.
            Some(Token::Field | Token::PercentPlus) => Ok(Command::Expr(self.additive()?)),
            // `| @(a; b)` — a list literal as the command's expression.
            Some(Token::At) if matches!(self.toks.get(self.pos + 1), Some(Token::LParen)) => {
                Ok(Command::Expr(self.additive()?))
            }
            Some(Token::At) => {
                // `| @-::prop` (and arithmetic over it) is a value
                // expression; `| @.` stays the whole-register recall.
                if matches!(
                    self.toks.get(self.pos + 1),
                    Some(Token::Name { text, quoted: false, .. }) if text == "-"
                ) {
                    return Ok(Command::Expr(self.additive()?));
                }
                self.pos += 1;
                self.expect_dot("@")?;
                Ok(Command::Recall(RegRef::Whole))
            }
            // A backtick literal is the sh(...) command, sugared
            // (Perl's qx): interpolation holes parameterize the
            // command per capsa.
            Some(Token::Shell(parts)) => {
                let parts = parts.clone();
                self.pos += 1;
                let arg = if let [lexer::InterpPart::Text(t)] = parts.as_slice() {
                    Arg::Lit(Value::Str(t.clone()))
                } else {
                    let mut segs = Vec::new();
                    for part in parts {
                        match part {
                            lexer::InterpPart::Text(t) => segs.push(InterpSeg::Text(t)),
                            lexer::InterpPart::Hole(src) => {
                                segs.push(self.parse_hole(&src)?);
                            }
                        }
                    }
                    Arg::Expr(Operand::Interp(segs))
                };
                Ok(Command::Func(FnCall {
                    name: "sh".into(),
                    args: vec![arg],
                }))
            }
            // The record sigil. `| %(...)` — the record
            // constructor, canonical spelling of `rec(...)`;
            // `| %.` — the named register view, as a record;
            // `| %%(...)` — the named view enriched with the given
            // fields (args after registers, args win on a shared
            // name); `| %%.` — the full view, anonymous regulae
            // included under their positions.
            Some(Token::Percent) => {
                self.pos += 1;
                if matches!(self.peek(), Some(Token::Percent)) {
                    self.pos += 1;
                    if matches!(self.peek(), Some(Token::LParen)) {
                        let call = self.record_args()?;
                        if call.args.is_empty() {
                            return Err(QuarbError::parse(
                                "an empty %%() adds nothing — the register view is %.",
                            ));
                        }
                        validate_record(&call)?;
                        return Ok(Command::RecordWith(call));
                    }
                    self.expect_dot("%%")?;
                    return Ok(Command::Recall(RegRef::FullRecord));
                }
                if matches!(self.peek(), Some(Token::LParen)) {
                    let call = self.record_args()?;
                    validate_record(&call)?;
                    return Ok(Command::Func(call));
                }
                self.expect_dot("%")?;
                Ok(Command::Recall(RegRef::Record))
            }
            // `| ...` — the spread. Dots are name characters, so
            // the ellipsis arrives as one name; it outranks the
            // dot-leading push reading.
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if text == "..." => {
                self.pos += 1;
                let outer = if matches!(self.peek(), Some(Token::Question { .. })) {
                    self.pos += 1;
                    true
                } else {
                    false
                };
                Ok(Command::Spread { outer })
            }
            // `quoted: false`: a quoted string that happens to start
            // with a dot (`| '.gitignore'`) is a constant topic, not
            // a push.
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if text.starts_with('.') => {
                let text = text.clone();
                self.pos += 1;
                let mut name = if text == "." {
                    None
                } else {
                    Some(text[1..].to_string())
                };
                // A push name is an identifier — it starts with a
                // letter — or a quoted string glued to the dot
                // (`."3"(…)`, the data-driven names a pivot writes).
                if name.is_none()
                    && let Some(Token::Name {
                        text: q,
                        quoted: true,
                        glued: true,
                    }) = self.peek()
                {
                    name = Some(q.clone());
                    self.pos += 1;
                }
                if let Some(n) = &name
                    && text.len() > 1
                    && !n.chars().next().is_some_and(|c| c.is_alphabetic())
                {
                    return Err(QuarbError::parse(format!(
                        "a push name starts with a letter ('| .name'); quote any other \
                         ('| .\"{n}\"'), and recall a position with $.N or ((.N))"
                    )));
                }
                // `.%(...)` / `.name%(...)` — the record push
                //: the constructor's fields, built and
                // filed in one step; `.%%(...)` the enriched view.
                if matches!(self.peek(), Some(Token::Percent)) {
                    self.pos += 1;
                    let enriched = if matches!(self.peek(), Some(Token::Percent)) {
                        self.pos += 1;
                        true
                    } else {
                        false
                    };
                    if !matches!(self.peek(), Some(Token::LParen)) {
                        // The bare `.%` is the fields push: the topic
                        // record's fields, each filed under its name.
                        if !enriched && name.is_none() {
                            return Ok(Command::FieldsPush);
                        }
                        return Err(QuarbError::parse(
                            "a record push takes its fields: `.%(…)` or `.name%(…)`; \
                             the bare `.%` (no name, no parens) spreads the topic record",
                        ));
                    }
                    let call = self.record_args()?;
                    validate_record(&call)?;
                    return Ok(Command::RecordPush {
                        name,
                        call,
                        enriched,
                    });
                }
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.pos += 1;
                    // A subcontext body is a navigating sub-query; a
                    // value expression (`.total(::price * ::qty)`) is
                    // the fallback when the query reading does not
                    // reach the closing parenthesis.
                    let save = self.pos;
                    self.subquery_depth += 1;
                    let tried = self.parse_query();
                    self.subquery_depth -= 1;
                    if let Ok(body) = tried
                        && matches!(self.peek(), Some(Token::RParen))
                    {
                        self.pos += 1;
                        return Ok(Command::Subcontext {
                            name,
                            body: Box::new(body),
                        });
                    }
                    self.pos = save;
                    // The push's own parentheses delimit the body, so
                    // a conditional needs no second pair:
                    // `.born(::quarter ? … : …)`.
                    let expr = self.cond_expr()?;
                    // …and neither does a pipe: `.a($_ | first)` is
                    // `.a(($_ | first))`.
                    let commands = self.inline_pipe_tail()?;
                    let expr = if commands.is_empty() {
                        expr
                    } else {
                        Operand::Piped {
                            expr: Box::new(expr),
                            commands,
                        }
                    };
                    self.expect(Token::RParen, "')' to close a subcontext")?;
                    Ok(Command::ExprPush { name, expr })
                } else {
                    // In navigation mode a bare push marks; marks
                    // may not take numeric names (positions number
                    // themselves).
                    // A mark named `_N` is reserved: `((_N))` is the
                    // join's N-th node in the rounded spelling.
                    if name.as_deref().is_some_and(|n| {
                        n.len() > 1
                            && n.starts_with('_')
                            && n[1..].chars().all(|c| c.is_ascii_digit())
                    }) {
                        return Err(QuarbError::parse(
                            "a mark named '_N' is reserved — ((_N)) is the join's N-th node ($$N)",
                        ));
                    }
                    Ok(Command::Push(name))
                }
            }
            // `| s/pat/repl/mods` — regex substitution on the topic.
            Some(Token::Subst {
                pattern,
                replacement,
                mods,
            }) => {
                // Validate the pattern at parse time.
                let case = if mods.contains('i') { "(?i)" } else { "" };
                Regex::new(&format!("{case}{pattern}"))
                    .map_err(|e| QuarbError::parse(format!("bad substitution pattern: {e}")))?;
                let call = FnCall {
                    name: "s".to_string(),
                    args: vec![
                        Arg::Lit(Value::Str(pattern.clone())),
                        Arg::Lit(Value::Str(replacement.clone())),
                        Arg::Lit(Value::Str(mods.clone())),
                    ],
                };
                self.pos += 1;
                Ok(Command::Func(call))
            }
            // `| [cond]` — a per-capsa filter. Positional selection
            // is a whole-context operation and lives on `@|`.
            Some(Token::LBracket) => {
                let positional = || {
                    QuarbError::parse(
                        "positional selection is whole-context; write '@| [n]' / \
                         '@| [a..b]' (a plain '| [cond]' filters per capsa)",
                    )
                };
                let mut e = match self.predicate()? {
                    Predicate::Expr(e) => e,
                    Predicate::Index(_) | Predicate::Range(_, _) => return Err(positional()),
                };
                // Glued predicates chain as on a path step:
                // `| [a][b]` ≡ `| [a] | [b]` — every capsa passes
                // both, in order.
                while matches!(self.peek(), Some(Token::LBracket)) {
                    match self.predicate()? {
                        Predicate::Expr(next) => e = PredExpr::And(Box::new(e), Box::new(next)),
                        Predicate::Index(_) | Predicate::Range(_, _) => return Err(positional()),
                    }
                }
                Ok(Command::Filter(e))
            }
            // A value-expression command starts with a projection, a
            // parenthesized group, or an interpolated string:
            // `| ::price * ::qty`, `| "${::name} (${::age})"`.
            // (A bare path start is a navigation command, handled
            // above; parenthesized paths remain expressions.)
            Some(
                Token::ColonColon
                | Token::ColonColonColon
                | Token::SemiSemiSemi
                | Token::LParen
                | Token::Interp(_),
            ) => Ok(Command::Expr(self.additive()?)),
            // A quoted string is a constant-topic command (`| 'text'`);
            // unquoted names remain command calls.
            Some(Token::Name { quoted: true, .. }) => Ok(Command::Expr(self.additive()?)),
            _ => {
                let call = self.func_call()?;
                // `now()` refuses command position: it takes no topic.
                if call.name == "now" {
                    return Err(QuarbError::parse(
                        "now() is a call operand (the invocation instant); it takes no \
                         topic — use it in expression position: [::date > now() - 12h]",
                    ));
                }
                func_command(call)
            }
        }
    }

    /// Whether the next tokens start a navigation command: an axis
    /// token, the root anchor, a `(name)` mark anchor followed by a
    /// path continuation, or an axis-led group (a quantified walk).
    /// A `(` that opens anything else stays an expression.
    fn nav_command_ahead(&self) -> bool {
        let axis = |t: &Token| {
            matches!(
                t,
                Token::Slash
                    | Token::SlashSlash
                    | Token::Backslash
                    | Token::BackslashBackslash
                    | Token::Gt { .. }
                    | Token::Lt { .. }
                    | Token::NextSibling
                    | Token::PrevSibling
                    | Token::FollowingSiblings(_)
                    | Token::PrecedingSiblings(_)
                    | Token::ArrowOut
                    | Token::ArrowIn
                    | Token::DashDash
            )
        };
        match self.peek() {
            Some(t) if axis(t) => true,
            Some(Token::Caret) => true,
            Some(Token::MarkOpen) => self.mark_anchor_ahead(),
            Some(Token::IdAnchorOpen) => true,
            Some(Token::LParen) => self.toks.get(self.pos + 1).is_some_and(axis),
            _ => false,
        }
    }

    /// Whether the next token continues a value expression after a
    /// complete operand — an arithmetic operator or a conditional
    /// `?`. Decides branch-vs-expression for a path-shaped command.
    fn expr_continues(&self) -> bool {
        match self.peek() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                matches!(text.as_str(), "+" | "-" | "*" | "div" | "idiv" | "mod")
            }
            Some(Token::Question { .. }) => true,
            // `| <-link:.v` — a path with a register accessor glued
            // to it is the cross-capsa read, an expression.
            Some(Token::Capsa(_)) => true,
            _ => false,
        }
    }

    /// Consume a `Name` that is exactly `.` (for `@.`).
    /// The dot that completes a register accessor (`@.`, `%.`,
    /// `%%.`); `sigil` names the one being read, for the message.
    fn expect_dot(&mut self, sigil: &str) -> Result<()> {
        match self.peek() {
            Some(Token::Name { text, .. }) if text == "." => {
                self.pos += 1;
                Ok(())
            }
            _ => Err(QuarbError::parse(format!("expected '.' after '{sigil}'"))),
        }
    }

    /// Parse one `||` branch: navigation and an optional projection.
    /// Whether the cursor sits on a `def` statement (`def &…`).
    fn at_def(&self) -> bool {
        matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "def")
            && matches!(self.toks.get(self.pos + 1), Some(Token::Amp))
    }

    /// Parse one `def &name(params): body;` statement into the
    /// fragment table. The body may invoke only fragments already
    /// defined, so recursion is impossible by construction.
    fn parse_def(&mut self) -> Result<()> {
        self.pos += 1; // 'def'
        self.expect(Token::Amp, "'&' after 'def'")?;
        let name = match self.bump() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => text.clone(),
            _ => {
                return Err(QuarbError::parse("expected a fragment name after 'def &'"));
            }
        };
        if self.defs.get(&name).is_some() {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' is already defined"
            )));
        }
        let mut params = Vec::new();
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            loop {
                self.expect(Token::Dollar, "'$' before a parameter name")?;
                let param = match self.bump() {
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) => text.clone(),
                    _ => {
                        return Err(QuarbError::parse("expected a parameter name after '$'"));
                    }
                };
                if param == "_"
                    || param == "ord"
                    || param == "ordinal"
                    || param.starts_with('.')
                    || param.starts_with('*')
                    || !param.chars().next().is_some_and(|c| c.is_alphabetic())
                {
                    return Err(QuarbError::parse(format!(
                        "parameter name '${param}' collides with a capsa-scope operand"
                    )));
                }
                params.push(param);
                if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            self.expect(Token::RParen, "')' to close the parameter list")?;
        }
        self.expect_separator("':' between the fragment name and its body")?;

        self.def_params = params.clone();
        // A body starting with a pipe is a pipeline fragment; one
        // starting with `[` is a predicate fragment (a reusable
        // guard); anything else is a navigation query (which may
        // carry a pipeline, and may be a correlation chain).
        let body = if matches!(self.peek(), Some(Token::LBracket)) {
            let mut preds = Vec::new();
            while matches!(self.peek(), Some(Token::LBracket)) {
                preds.push(self.predicate()?);
            }
            DefBody::Predicates(preds)
        } else if matches!(self.peek(), Some(Token::Pipe | Token::At)) {
            let mut commands = Vec::new();
            // A fragment body's splice-time mode is unknowable at
            // definition time; parse permissively (navigation mode)
            // — the splice site's own commands still typecheck.
            self.pipeline_items(&mut commands, PipeMode::Nav)?;
            if commands.is_empty() {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' has an empty body"
                )));
            }
            DefBody::Pipeline(commands)
        } else {
            // A command-shaped body without its pipe is the classic
            // slip — point at the spelling instead of the grammar.
            if matches!(self.peek(),
                Some(Token::Name { text, quoted: false, .. }) if text.starts_with('.'))
                && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
            {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' body looks like a pipeline command; \
                     a pipeline fragment starts with its pipe: \
                     'def &{name}: | .push(...) ;'"
                )));
            }
            // Placement of correlated references is checked at the
            // splice site, where the join's shape is known — not
            // here.
            DefBody::Query(self.parse_chain().map_err(|e| {
                QuarbError::parse(format!(
                    "in fragment '&{name}' body: {e} (a pipeline \
                     fragment's body starts with its pipe: \
                     'def &{name}: | ...')"
                ))
            })?)
        };
        self.def_params.clear();
        self.expect(Token::Semi, "';' to end the definition")?;
        self.defs.entries.push((
            name,
            Def {
                params,
                rest: None,
                data_aware: false,
                body,
            },
        ));
        Ok(())
    }

    fn at_macro(&self) -> bool {
        matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "macro")
            && matches!(self.toks.get(self.pos + 1), Some(Token::Amp))
    }

    /// Parse one `macro &name(params): body;` statement. The body is
    /// a query evaluated at expansion time against the invocation's
    /// expansion arbor (one child per parameter: the argument form's
    /// reflected subtree); its text results, joined, become the
    /// expansion. Parameters are `$name` forms; a trailing `@name`
    /// rest-parameter collects the remaining arguments.
    fn parse_macro(&mut self) -> Result<()> {
        self.pos += 1; // 'macro'
        self.expect(Token::Amp, "'&' after 'macro'")?;
        let name = match self.bump() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => text.clone(),
            _ => {
                return Err(QuarbError::parse("expected a macro name after 'macro &'"));
            }
        };
        // `macro &name!` declares a data-aware macro: its body reads
        // the dataset (mounted as `/data`), and every invocation
        // spells the `!`.
        let data_aware = matches!(self.peek(), Some(Token::Bang));
        if data_aware {
            self.pos += 1;
        }
        if self.defs.get(&name).is_some() {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' is already defined"
            )));
        }
        let mut params = Vec::new();
        let mut rest = None;
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            loop {
                let is_rest = match self.peek() {
                    Some(Token::Dollar) => false,
                    Some(Token::At) => true,
                    _ => {
                        return Err(QuarbError::parse(
                            "expected '$name' or a trailing '@rest' parameter",
                        ));
                    }
                };
                self.pos += 1;
                let param = match self.bump() {
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) => text.clone(),
                    _ => {
                        return Err(QuarbError::parse(
                            "expected a parameter name after its sigil",
                        ));
                    }
                };
                if param == "_"
                    || param == "ord"
                    || param == "ordinal"
                    || param.starts_with('.')
                    || param.starts_with('*')
                    || !param.chars().next().is_some_and(|c| c.is_alphabetic())
                {
                    return Err(QuarbError::parse(format!(
                        "parameter name '{param}' collides with a capsa-scope operand"
                    )));
                }
                if data_aware && param == "data" {
                    return Err(QuarbError::parse(
                        "a data-aware macro mounts the dataset as '/data'; \
                         pick another parameter name",
                    ));
                }
                if is_rest {
                    rest = Some(param);
                    break;
                }
                params.push(param);
                if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            self.expect(Token::RParen, "')' to close the parameter list")?;
        }
        self.expect_separator("':' between the macro name and its body")?;
        if matches!(self.peek(), Some(Token::Pipe | Token::At)) {
            return Err(QuarbError::parse(format!(
                "a macro body is a query over its expansion arbor; anchor a \
                 non-navigating body at the root: 'macro &{name}: ^ | ...;'"
            )));
        }
        self.def_params = params.clone();
        let body = self.parse_query()?;
        self.def_params.clear();
        self.expect(Token::Semi, "';' to end the definition")?;
        self.defs.entries.push((
            name,
            Def {
                params,
                rest,
                data_aware,
                body: DefBody::Macro(body),
            },
        ));
        Ok(())
    }

    /// Ruling #22 (invited capture), the recording half: after a
    /// macro expansion parses, note every register name its
    /// generated text pushes that no argument invited, with the
    /// expansion's own usage counts. The sweep at end of parse
    /// compares them against whole-unit usage.
    fn record_captures(
        &self,
        name: &str,
        invited: &std::collections::BTreeSet<String>,
        usage: crate::reflect::RegUsage,
    ) {
        if usage.pushes.is_empty() {
            return;
        }
        let mut caps = self.captures.borrow_mut();
        for (reg, n) in &usage.pushes {
            if invited.contains(reg) {
                continue;
            }
            caps.push(CaptureRec {
                mac: name.to_string(),
                reg: reg.clone(),
                pushes: *n,
                recalls: usage.recalls.get(reg).copied().unwrap_or(0),
            });
        }
    }

    /// Ruling #22, the sweep: an uninvited macro-pushed register
    /// name must not be used anywhere else in this parse unit —
    /// whole-unit push/recall counts may not exceed what the
    /// recorded expansions themselves contributed. Deliberate
    /// anaphora stays legal by riding an argument (the `aif`
    /// shape); the accidental-capture pitfall (LISP's broken
    /// `swap`, the reason gensym exists) refuses with the invite
    /// spelled out.
    fn sweep_captures(&self, q: &Query) -> Result<()> {
        let caps = self.captures.borrow();
        if caps.is_empty() {
            return Ok(());
        }
        let global = crate::reflect::usage_of_query(q);
        let mut own: std::collections::BTreeMap<&str, (usize, usize, &str)> =
            std::collections::BTreeMap::new();
        for c in caps.iter() {
            let e = own.entry(c.reg.as_str()).or_insert((0, 0, c.mac.as_str()));
            e.0 += c.pushes;
            e.1 += c.recalls;
        }
        for (reg, (_p, r, mac)) in own {
            // The hazard is a recall OUTSIDE the expansion binding
            // to the macro's push. Surrounding pushes of the same
            // name stay legal (ordinary shadowing, nothing reads
            // through the macro's push) — recall counts alone
            // decide, so a pivot-style push sweep feeding `%.`
            // never trips the lint.
            let gr = global.recalls.get(reg).copied().unwrap_or(0);
            if gr > r {
                return Err(QuarbError::parse(format!(
                    "macro '&{mac}' pushes the register '.{reg}', and the \
                     surrounding query recalls '$.{reg}' — capture must \
                     be invited through an argument: pass '$.{reg}' (or \
                     the bare name) to '&{mac}', or rename the emitted \
                     push"
                )));
            }
        }
        Ok(())
    }

    /// Expand a macro invocation to query text: bind arguments
    /// (literals by value, forms by their unparsed text), build the
    /// expansion arbor, run the body against it, and join the text
    /// results.
    fn expand_macro_text(&self, name: &str, def: &Def, args: Vec<Operand>) -> Result<String> {
        let values = self.expand_macro_values(name, def, args)?;
        let text = values.join(" ");
        if text.trim().is_empty() {
            return Err(QuarbError::expansion(format!(
                "macro '&{name}' expanded to nothing"
            )));
        }
        self.first_steps.borrow_mut().push(text.clone());
        Ok(text)
    }

    /// Expand a macro invocation at a path or operand position:
    /// join like [`Self::expand_macro_text`], but first refuse the
    /// silent-concatenation trap — several branch-shaped output
    /// values would space-join into one accidental deep path
    /// (`/a /b` reads as `/a/b`, which runs and finds nothing).
    fn expand_macro_path_text(&self, name: &str, def: &Def, args: Vec<Operand>) -> Result<String> {
        let values = self.expand_macro_values(name, def, args)?;
        let nav_start = |v: &str| {
            let t = v.trim_start();
            ["/", "\\", "->", "<-", "--", "(", "^", ">", "<"]
                .iter()
                .any(|op| t.starts_with(op))
        };
        if values.len() >= 2 && values.iter().all(|v| nav_start(v)) {
            return Err(QuarbError::expansion(format!(
                "macro '&{name}' expanded to {} branch-shaped values; \
                 space-joined they would read as one path — join them \
                 into a union (@| join(' || ')) or emit a single group",
                values.len()
            )));
        }
        let text = values.join(" ");
        if text.trim().is_empty() {
            return Err(QuarbError::expansion(format!(
                "macro '&{name}' expanded to nothing"
            )));
        }
        self.first_steps.borrow_mut().push(text.clone());
        Ok(text)
    }

    /// The expansion's raw output values (each `.to_string()`ed),
    /// before joining — see [`Self::expand_macro_text`].
    fn expand_macro_values(
        &self,
        name: &str,
        def: &Def,
        args: Vec<Operand>,
    ) -> Result<Vec<String>> {
        let n = def.params.len();
        let arity_ok = match def.rest {
            Some(_) => args.len() >= n,
            None => args.len() == n,
        };
        if !arity_ok {
            return Err(QuarbError::parse(format!(
                "macro '&{name}' takes {}{} argument(s), got {}",
                n,
                if def.rest.is_some() { "+" } else { "" },
                args.len()
            )));
        }
        // Outside interpolation holes a parameter is the argument
        // form itself (call-by-name, as in template fragments) —
        // `| $col` projects the data by the argument column. Inside
        // a hole it splices as its canonical text (literals: their
        // value) — `${$col}` writes the form into generated query
        // text.
        let mut forms = HashMap::new();
        let mut texts = HashMap::new();
        for (p, a) in def.params.iter().zip(&args) {
            forms.insert(p.clone(), a.clone());
            let text = match a {
                Operand::Lit(v) => Operand::Lit(v.clone()),
                form => Operand::Lit(Value::Str(crate::unparse::operand_text(form))),
            };
            texts.insert(p.clone(), text);
        }
        let DefBody::Macro(body) = &def.body else {
            unreachable!("checked by caller");
        };
        let mut body = body.clone();
        subst_query(
            &mut body,
            &Subst {
                outer: &forms,
                hole: &texts,
            },
        );

        let mut bindings: Vec<(String, crate::reflect::MacroBinding)> = def
            .params
            .iter()
            .zip(&args)
            .map(|(p, a)| (p.clone(), crate::reflect::MacroBinding::One(a.clone())))
            .collect();
        if let Some(rest) = &def.rest {
            bindings.push((
                rest.clone(),
                crate::reflect::MacroBinding::Rest(args[n..].to_vec()),
            ));
        }
        let arbor = crate::reflect::expansion_arbor(&bindings);
        // A data-aware macro reads the dataset, mounted as `/data`
        // beside the parameters; a pure macro sees the forms alone.
        // The body is evaluated *here*, at expansion time, so its
        // shell command (`sh(...)` or a backtick) must clear the same
        // `--allow-shell` gate the top-level query does — otherwise a
        // macro body could run a command with no opt-in. The gate is
        // the run's (`Run::allow_shell`; parsing outside a run is
        // gated); the nested evaluation inherits the run's options.
        let run = crate::exec::run_option(|o| o.clone());
        let result = if def.data_aware {
            let Some(data) = self.data else {
                return Err(QuarbError::parse(format!(
                    "macro '&{name}!' is data-aware: its expansion reads \
                     the dataset, so it needs an input (it cannot expand \
                     from query text alone)"
                )));
            };
            let combined = crate::reflect::ExpansionAdapter::new(arbor, data);
            crate::exec::gate(&body, &run)?;
            crate::exec::eval(&body, &combined)
        } else {
            crate::exec::gate(&body, &run)?;
            crate::exec::eval(&body, &arbor)
        };
        let values = match result {
            crate::exec::QueryResult::Values(vs) => vs,
            crate::exec::QueryResult::Nodes(_) => {
                return Err(QuarbError::parse(format!(
                    "macro '&{name}' must produce query text; its body \
                     returned nodes (project or interpolate)"
                )));
            }
        };
        Ok(values.iter().map(|v| v.to_string()).collect())
    }

    /// Parse `&name`, `&name(arg, …)`, or the data-aware `&name!(…)`
    /// and return the name, the argument forms, whether the `!` was
    /// spelled, and a glued quantifier. `+`/`*` are name characters,
    /// so `&clean+` lexes as one name token — when the full name is
    /// not in the ledger but the stripped one is, the tail is the
    /// splice's quantifier (path positions consume it; the others
    /// refuse it).
    fn invocation(&mut self) -> Result<(String, Vec<Operand>, bool, Option<Quant>)> {
        self.expect(Token::Amp, "'&'")?;
        let mut name = match self.bump() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => text.clone(),
            _ => {
                return Err(QuarbError::parse("expected a fragment name after '&'"));
            }
        };
        let mut quant = None;
        if self.defs.get(&name).is_none()
            && let Some(stripped) = name.strip_suffix(['+', '*'])
            && self.defs.get(stripped).is_some()
        {
            quant = Some(if name.ends_with('+') {
                Quant { min: 1, max: None }
            } else {
                Quant { min: 0, max: None }
            });
            name = stripped.to_string();
        }
        let bang = matches!(self.peek(), Some(Token::Bang));
        if bang {
            self.pos += 1;
        }
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            if !matches!(self.peek(), Some(Token::RParen)) {
                loop {
                    // Same relaxation as command operands: a
                    // conditional argument stands bare.
                    args.push(self.cond_expr()?);
                    if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen, "')' to close fragment arguments")?;
        }
        Ok((name, args, bang, quant))
    }

    /// Enforce the `!` signage both ways: a data-aware macro must be
    /// invoked as `&name!`, and nothing else may carry the bang.
    fn check_bang(&self, name: &str, def: &Def, bang: bool) -> Result<()> {
        if def.data_aware && !bang {
            return Err(QuarbError::parse(format!(
                "macro '&{name}' is data-aware (its expansion reads the \
                 dataset); invoke it as '&{name}!(...)'"
            )));
        }
        if !def.data_aware && bang {
            return Err(QuarbError::parse(format!(
                "'!' marks data-aware macros; '&{name}' is pure — invoke \
                 it without the '!'"
            )));
        }
        Ok(())
    }

    /// Bind invocation arguments to a fragment's parameters.
    fn bind(
        &self,
        name: &str,
        params: &[String],
        args: Vec<Operand>,
    ) -> Result<HashMap<String, Operand>> {
        if args.len() != params.len() {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' takes {} argument(s), got {}",
                params.len(),
                args.len()
            )));
        }
        Ok(params.iter().cloned().zip(args).collect())
    }

    /// Expand a query-fragment invocation into its (substituted)
    /// query, returning the fragment's name and glued quantifier
    /// for splice-site handling.
    fn invoke_query_fragment(&mut self) -> Result<(String, Query, Option<Quant>)> {
        let (name, args, bang, quant) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::parse(format!("unknown fragment '&{name}'")));
        };
        self.check_bang(&name, &def, bang)?;
        let q = match def.body {
            DefBody::Query(mut q) => {
                let map = self.bind(&name, &def.params, args)?;
                subst_query(
                    &mut q,
                    &Subst {
                        outer: &map,
                        hole: &map,
                    },
                );
                q
            }
            DefBody::Pipeline(_) => {
                return Err(QuarbError::parse(format!(
                    "'&{name}' is a pipeline fragment; invoke it after a pipe"
                )));
            }
            DefBody::Predicates(_) => {
                return Err(QuarbError::parse(format!(
                    "'&{name}' is a predicate fragment; it refines the \
                     step before it ('/div&{name}') or reads as a \
                     condition ('[&{name}]')"
                )));
            }
            // A macro expands to text, reparsed here as a query.
            DefBody::Macro(_) => {
                let invited = invites_of(&args);
                let text = self.expand_macro_path_text(&name, &def, args)?;
                let wrap = |e: QuarbError| {
                    QuarbError::expansion(format!("in expansion of '&{name}' ('{text}')"))
                        .wrapping(e)
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                if matches!(tokens.first(), Some(Token::Pipe | Token::At)) {
                    return Err(QuarbError::expansion(format!(
                        "macro '&{name}' expanded to a pipeline fragment \
                         ('{text}'); invoke it after a pipe"
                    )));
                }
                let q =
                    parse_with_data(&tokens, self.defs.before(&name), self.data).map_err(wrap)?;
                self.record_captures(&name, &invited, crate::reflect::usage_of_query(&q));
                q
            }
        };
        Ok((name, q, quant))
    }

    /// Expand a pipeline-fragment invocation onto `pipeline`. The
    /// invoking pipe must match the fragment's first pipe.
    fn invoke_pipeline_fragment(
        &mut self,
        pipe: &'static str,
        pipeline: &mut Vec<Command>,
    ) -> Result<()> {
        let (name, args, bang, quant) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::parse(format!("unknown fragment '&{name}'")));
        };
        if quant.is_some() {
            return Err(QuarbError::parse(format!(
                "a quantifier doesn't ride the pipe; quantify '&{name}' \
                 at path position"
            )));
        }
        self.check_bang(&name, &def, bang)?;
        match def.body {
            DefBody::Pipeline(commands) => {
                let first = command_pipe(commands.first().expect("non-empty checked at def time"));
                if first != pipe {
                    return Err(QuarbError::parse(format!(
                        "fragment '&{name}' begins with '{first}' but was invoked with '{pipe}'"
                    )));
                }
                let map = self.bind(&name, &def.params, args)?;
                let subst = Subst {
                    outer: &map,
                    hole: &map,
                };
                for mut command in commands {
                    subst_command(&mut command, &subst);
                    pipeline.push(command);
                }
                Ok(())
            }
            DefBody::Query(_) => Err(QuarbError::parse(format!(
                "'&{name}' is a query fragment; invoke it at path position"
            ))),
            // A predicate fragment on the plain pipe is the guard as
            // a per-capsa filter — `| &vis` ≡ `| [cond]` per predicate.
            DefBody::Predicates(preds) => {
                if pipe != "|" {
                    return Err(QuarbError::parse(format!(
                        "'@|' aggregates the whole context; the predicate \
                         fragment '&{name}' filters per capsa — invoke it \
                         with '|'"
                    )));
                }
                let map = self.bind(&name, &def.params, args)?;
                let subst = Subst {
                    outer: &map,
                    hole: &map,
                };
                for pred in preds {
                    match pred {
                        Predicate::Expr(mut e) => {
                            subst_pred_expr(&mut e, &subst);
                            pipeline.push(Command::Filter(e));
                        }
                        _ => {
                            return Err(QuarbError::parse(format!(
                                "the predicate fragment '&{name}' holds a \
                                 positional predicate; select positionally \
                                 with '@| [n]'"
                            )));
                        }
                    }
                }
                Ok(())
            }
            // A macro expands to text; here it must be a command
            // sequence whose first pipe matches the invocation.
            DefBody::Macro(_) => {
                let invited = invites_of(&args);
                let text = self.expand_macro_text(&name, &def, args)?;
                let wrap = |e: QuarbError| {
                    QuarbError::expansion(format!("in expansion of '&{name}' ('{text}')"))
                        .wrapping(e)
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                let first = match tokens.first() {
                    Some(Token::Pipe) => "|",
                    Some(Token::At) => "@|",
                    _ => {
                        return Err(QuarbError::parse(format!(
                            "macro '&{name}' expanded to a query fragment \
                             ('{text}'); invoke it at path position — or \
                             have the body emit a leading '| ' to splice \
                             as pipeline commands"
                        )));
                    }
                };
                if first != pipe {
                    return Err(QuarbError::parse(format!(
                        "macro '&{name}' expanded to a '{first}' pipeline \
                         ('{text}') but was invoked with '{pipe}'"
                    )));
                }
                let mut p = self.child(&tokens, self.defs.before(&name))?;
                let mut commands = Vec::new();
                p.pipeline_items(&mut commands, PipeMode::Nav)
                    .map_err(wrap)?;
                if p.pos != tokens.len() {
                    return Err(QuarbError::parse(format!(
                        "macro '&{name}' expanded to text with trailing \
                         content ('{text}')"
                    )));
                }
                self.record_captures(
                    &name,
                    &invited,
                    crate::reflect::usage_of_commands(&commands),
                );
                pipeline.extend(commands);
                Ok(())
            }
        }
    }

    fn branch(&mut self) -> Result<Branch> {
        // Optional explicit anchor (the default is the current
        // node, which at the top level is the root). A lone `^` is
        // a complete branch: the root itself as the context
        // (`^ | count`, non-navigating macro bodies). The mark
        // anchors — `(name)`, `(N)`, `(.)`, `(@)`, `(@name)` —
        // read as anchors only in the exact shapes peek_anchor
        // accepts; groups keep `(` for everything else.
        // `*//name` — the universal search: a branch head only,
        // root-anchored, its first step over the whole arbor.
        if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "*")
            && matches!(self.toks.get(self.pos + 1), Some(Token::SlashSlash))
        {
            self.pos += 1;
            let mut elem = self.path_elem()?;
            universalize(&mut elem)?;
            return self.branch_tail(Anchor::Root, vec![elem], None);
        }
        let anchor = if matches!(self.peek(), Some(Token::Caret)) {
            self.pos += 1;
            Anchor::Root
        } else {
            self.mark_anchor().unwrap_or(Anchor::Current)
        };
        self.branch_tail(anchor, Vec::new(), None)
    }

    /// A branch's step loop and projection, continuing from
    /// already-spliced elements (empty for a plain branch). A
    /// projection carried in by a splice ends the walk immediately.
    fn branch_tail(
        &mut self,
        anchor: Anchor,
        mut steps: Vec<PathElem>,
        mut projection: Option<Projection>,
    ) -> Result<Branch> {
        while projection.is_none()
            && let Some(tok) = self.peek()
        {
            if matches!(
                tok,
                Token::Pipe
                    | Token::PipePipe
                    | Token::At
                    | Token::RParen
                    | Token::Correlate
                    | Token::Semi
                    // `X:.r` — a register accessor glued to the
                    // path closes the walk: the cross-capsa read.
                    | Token::Capsa(_)
            ) {
                break;
            }
            // A fragment invocation splices into the walk.
            if matches!(tok, Token::Amp) {
                projection = self.splice_path_fragment(&mut steps, SplicePos::MidPath)?;
                continue;
            }
            // The union word (`.or.` and the family's spellings)
            // ends the branch as `||` does — a hop named so is
            // written with its axis or quoted.
            if self.peek_word(OR_WORDS) {
                break;
            }
            // `$|` at pipeline level is the map pipe — a command, not a
            // path step; end the branch so `pipeline_items` sees it.
            // `finish_step` deliberately leaves this `$` unconsumed
            // (it would otherwise become a spurious leaf anchor).
            if matches!(tok, Token::Dollar)
                && self.pattern_depth == 0
                && matches!(self.toks.get(self.pos + 1), Some(Token::Pipe))
            {
                break;
            }
            if is_projection_start(tok) {
                // `::name-->` is a resolution step (navigation
                // continues); any other `::` ends the branch.
                if self.is_resolution_ahead() {
                    steps.push(self.path_elem()?);
                    continue;
                }
                break;
            }
            steps.push(self.path_elem()?);
        }

        if projection.is_none() {
            projection = self.projection()?;
        }

        if steps.is_empty() && projection.is_none() && anchor == Anchor::Current {
            return Err(QuarbError::parse(
                "a query branch needs at least one step or a projection",
            ));
        }
        Ok(Branch {
            steps,
            projection,
            anchor,
        })
    }

    /// Expand a fragment invocation at a path position and splice it
    /// into the walk: elements extend `steps` (a predicate fragment
    /// instead refines the element just walked), and a projection
    /// carried by the body ends the branch. Trailing refinement — a
    /// quantifier, predicates, a reach mark — group-wraps the
    /// spliced elements.
    fn splice_path_fragment(
        &mut self,
        steps: &mut Vec<PathElem>,
        at: SplicePos,
    ) -> Result<Option<Projection>> {
        let (name, args, bang, quant) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::parse(format!("unknown fragment '&{name}'")));
        };
        self.check_bang(&name, &def, bang)?;
        let q = match def.body {
            DefBody::Predicates(preds) => {
                if quant.is_some() {
                    return Err(QuarbError::parse(format!(
                        "the predicate fragment '&{name}' refines what \
                         precedes it; a quantifier cannot ride it"
                    )));
                }
                let map = self.bind(&name, &def.params, args)?;
                self.attach_predicate_fragment(&name, preds, &map, steps)?;
                // Bracket predicates written after the guard belong
                // to the same element — `&vis[p]` is one predicate
                // run, like `[vis][p]` spelled by hand.
                while matches!(self.peek(), Some(Token::LBracket)) {
                    let p = self.predicate()?;
                    self.attach_predicate_fragment(&name, vec![p], &HashMap::new(), steps)?;
                }
                return Ok(None);
            }
            DefBody::Pipeline(_) => {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' carries a pipeline; a pipe leaves \
                     navigation — invoke it at branch head or as a \
                     pipeline command, not inside a walk"
                )));
            }
            DefBody::Query(mut q) => {
                let map = self.bind(&name, &def.params, args)?;
                subst_query(
                    &mut q,
                    &Subst {
                        outer: &map,
                        hole: &map,
                    },
                );
                q
            }
            DefBody::Macro(_) => {
                let invited = invites_of(&args);
                let text = self.expand_macro_path_text(&name, &def, args)?;
                let q = self.parse_expansion_query(&name, &text)?;
                self.record_captures(&name, &invited, crate::reflect::usage_of_query(&q));
                q
            }
        };
        // The category rule at path positions: the body must be pure
        // navigation, continuing the walk from where it stands.
        if !q.correlations.is_empty() {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' carries a correlation; a chain \
                 splices at driver position only"
            )));
        }
        if !q.pipeline.is_empty() {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' carries a pipeline; a pipe leaves \
                 navigation — invoke it at branch head or as a pipeline \
                 command, not inside a walk"
            )));
        }
        if q.branches.iter().any(|b| b.anchor != Anchor::Current) {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' re-anchors; mid-path splicing \
                 continues the walk — invoke it at branch head"
            )));
        }
        let (elems, projection) = self.finish_splice(&name, q.branches, quant, at)?;
        steps.extend(elems);
        Ok(projection)
    }

    /// Turn a spliced body's branches into walk elements and apply
    /// any trailing refinement. A union body splices as a
    /// path-pattern group, one alternative per branch — under a
    /// quantifier, alternation takes the group's simple-path
    /// semantics.
    fn finish_splice(
        &mut self,
        name: &str,
        branches: Vec<Branch>,
        glued_quant: Option<Quant>,
        at: SplicePos,
    ) -> Result<(Vec<PathElem>, Option<Projection>)> {
        let (mut elems, projection) = if branches.len() == 1 {
            let b = branches.into_iter().next().expect("non-empty");
            (b.steps, b.projection)
        } else {
            if branches.iter().any(|b| b.projection.is_some()) {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' is a projected union; it splices \
                     whole at branch head, not inside a walk"
                )));
            }
            let alts: Vec<Vec<PathElem>> = branches.into_iter().map(|b| b.steps).collect();
            (
                vec![PathElem::Group(Group {
                    alts,
                    quant: Quant {
                        min: 1,
                        max: Some(1),
                    },
                    predicates: Vec::new(),
                    traits: Vec::new(),
                    reach: Reach::All,
                })],
                None,
            )
        };

        // Trailing refinement: `&clean+` ≡ `(…)+`, `&m[p]` ≡ `(…)[p]`
        // (expression predicates only, like any group). A glued
        // quantifier rode in on the name token; a brace one is its
        // own token, parsed here.
        let quant = match glued_quant {
            Some(q) => Some(q),
            None => self.group_quant()?,
        };
        if quant.is_some() || matches!(self.peek(), Some(Token::LBracket)) {
            if projection.is_some() {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' ends in a projection; refinement \
                     cannot follow it — refine through the pipe instead"
                )));
            }
            let predicates = self.group_predicates()?;
            let reach = self.reach();
            elems = group_wrap(
                elems,
                quant.unwrap_or(Quant {
                    min: 1,
                    max: Some(1),
                }),
                predicates,
                reach,
            );
        }

        if projection.is_some() {
            let continues = matches!(
                self.peek(),
                Some(
                    Token::Slash
                        | Token::SlashSlash
                        | Token::Backslash
                        | Token::BackslashBackslash
                        | Token::ArrowOut
                        | Token::ArrowIn
                        | Token::DashDash
                        | Token::LParen
                        | Token::Amp
                        | Token::ColonColon
                        | Token::ColonColonColon
                        | Token::SemiSemiSemi
                )
            );
            if at == SplicePos::GroupAlt {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' ends in a projection; a group \
                     alternative walks on — invoke it where the branch ends"
                )));
            }
            if continues {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' ends in a projection; navigation \
                     cannot continue past it — invoke it where the branch \
                     ends"
                )));
            }
        }
        // `<trait>` after an invocation was never legal and stays so
        // — a splice has no single step to carry traits. (`&frag<sib`,
        // the previous-sibling axis, continues the walk as usual: the
        // trait shape is `<name>` with nothing walkable after.)
        if matches!(self.peek(), Some(Token::Lt { .. }))
            && matches!(self.toks.get(self.pos + 1), Some(Token::Name { .. }))
            && matches!(self.toks.get(self.pos + 2), Some(Token::Gt { .. }))
            && !matches!(
                self.toks.get(self.pos + 3),
                Some(Token::Name { .. } | Token::Regex(_))
            )
        {
            return Err(QuarbError::parse(
                "a fragment does not take a trailing trait selector; \
                 put the trait inside a definition: \
                 'def &errs: /entry<error> ;'",
            ));
        }
        Ok((elems, projection))
    }

    /// Splice a predicate fragment: its predicates refine the
    /// element just walked (a step, or a group's match set).
    fn attach_predicate_fragment(
        &self,
        name: &str,
        mut preds: Vec<Predicate>,
        map: &HashMap<String, Operand>,
        steps: &mut [PathElem],
    ) -> Result<()> {
        let subst = Subst {
            outer: map,
            hole: map,
        };
        for pred in &mut preds {
            if let Predicate::Expr(e) = pred {
                subst_pred_expr(e, &subst);
            }
        }
        match steps.last_mut() {
            Some(PathElem::Step(s)) => {
                s.predicates.extend(preds);
                Ok(())
            }
            Some(PathElem::Group(g)) => {
                if preds.iter().any(|p| !matches!(p, Predicate::Expr(_))) {
                    return Err(QuarbError::parse(
                        "a group takes expression predicates only \
                         (positional selection has no order across \
                         repetition tiers)",
                    ));
                }
                g.predicates.extend(preds);
                Ok(())
            }
            Some(PathElem::Mark(_) | PathElem::Push { .. }) => Err(QuarbError::parse(format!(
                "the predicate fragment '&{name}' refines a hop or a \
                 group; a mark or a push takes no predicates"
            ))),
            None => Err(QuarbError::parse(format!(
                "the predicate fragment '&{name}' refines the step \
                 before it; nothing precedes it here — walk first \
                 ('/div&{name}')"
            ))),
        }
    }

    /// Re-parse a macro's expansion text at a path position: a full
    /// program (definitions allowed), against the ledger as it stood
    /// before the macro, with the splice site's predicate and
    /// pattern scopes inherited — generated text obeys the same
    /// contextual restrictions hand-written text would.
    fn parse_expansion_query(&self, name: &str, text: &str) -> Result<Query> {
        let wrap = |e: QuarbError| {
            QuarbError::expansion(format!("in expansion of '&{name}' ('{text}')")).wrapping(e)
        };
        let tokens = lexer::lex(text).map_err(wrap)?;
        if matches!(tokens.first(), Some(Token::Pipe | Token::At)) {
            return Err(QuarbError::expansion(format!(
                "macro '&{name}' expanded to a pipeline fragment \
                 ('{text}'); at path position it must expand to \
                 navigation steps"
            )));
        }
        let mut p = self.child(&tokens, self.defs.before(name))?;
        p.pattern_depth = self.pattern_depth;
        p.predicate_depth = self.predicate_depth;
        p.parse().map_err(wrap)
    }

    /// Peek-only form of [`Self::mark_anchor`], for match guards.
    fn mark_anchor_ahead(&self) -> bool {
        self.peek_anchor().is_some() || matches!(self.peek(), Some(Token::IdAnchorOpen))
    }

    /// The pointer for the retired single-paren anchor: `(name)`,
    /// `(N)` followed by a path, or `(@)` / `(@name)` — the value
    /// side's parentheses on a node. `Err` when the shape is seen.
    fn refuse_single_paren_anchor(&self) -> Result<()> {
        if matches!(self.toks.get(self.pos), Some(Token::LParen))
            && let Some((anchor, _)) = self.anchor_shape()
        {
            let spelled = match anchor {
                Anchor::MarksAll => "((@))".to_string(),
                Anchor::MarksNamed(n) => format!("((@{n}))"),
                Anchor::Id(op) => format!("((!{}))", crate::unparse::operand_text(&op)),
                Anchor::Mark(n) => format!("$$.{n}"),
                Anchor::MarkIndex(n) => format!("$$.{n}"),
                _ => "$$.".to_string(),
            };
            return Err(QuarbError::parse(format!(
                "a mark anchor is a node: write '{spelled}' (single parentheses are the value side)"
            )));
        }
        Ok(())
    }

    /// The mark-anchor lookahead: `(name)` / `(N)` / `(.)` one
    /// mark, `(@)` / `(@name)` the plural. The name and number
    /// interiors require a path continuation after the closing
    /// paren — without one they stay a parenthesized expression
    /// (`(1) * 2` is arithmetic). The dot and at interiors cannot
    /// be expressions, so they stand on their own (`| (@)` alone
    /// re-seeds a thread from its marks). Returns the anchor and
    /// its token span without consuming.
    fn peek_anchor(&self) -> Option<(Anchor, usize)> {
        if !matches!(self.toks.get(self.pos), Some(Token::MarkOpen)) {
            return None;
        }
        self.anchor_shape()
    }

    /// The anchor interior after an opener at `pos` — shared by the
    /// live `((…))` lookahead and the retired `(…)` pointer.
    fn anchor_shape(&self) -> Option<(Anchor, usize)> {
        let continues = |i: usize| {
            matches!(
                self.toks.get(i),
                Some(
                    Token::Slash
                        | Token::SlashSlash
                        | Token::Backslash
                        | Token::BackslashBackslash
                        | Token::ArrowOut
                        | Token::ArrowIn
                        | Token::DashDash
                        | Token::ColonColon
                        | Token::ColonColonColon
                        | Token::SemiSemiSemi
                )
            )
        };
        // The live opener (`$$.name`, `((name))`) can be nothing but
        // an anchor, so it stands with or without a continuation —
        // bare, it is the marked node itself (`| $$.e` re-seats the
        // thread on it). The continuation is asked for only where
        // the interior could be an expression: the retired single
        // parenthesis.
        let live = matches!(self.toks.get(self.pos), Some(Token::MarkOpen));
        let continues = |i: usize| live || continues(i);
        // A mark name is word-shaped: it may not start with a
        // digit (positions number themselves), a dot, or any
        // operator character that would collide with an
        // expression reading (`(@*)` is the parenthesized capsae
        // operand, never an anchor).
        let word = |text: &str| {
            text.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
                && !text.contains('/')
        };
        match self.toks.get(self.pos + 1) {
            // `(@)` all marks; `(@name)` all marks under a name.
            Some(Token::At) => match self.toks.get(self.pos + 2) {
                Some(Token::RParen) => Some((Anchor::MarksAll, 3)),
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if word(text) && matches!(self.toks.get(self.pos + 3), Some(Token::RParen)) => {
                    Some((Anchor::MarksNamed(text.clone()), 4))
                }
                _ => None,
            },
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if matches!(self.toks.get(self.pos + 2), Some(Token::RParen)) => {
                if text == "." {
                    // `(.)` — the latest mark (the array's top).
                    Some((Anchor::MarkTop, 3))
                } else if text.chars().all(|c| c.is_ascii_digit()) {
                    if continues(self.pos + 3) {
                        text.parse()
                            .ok()
                            .filter(|&n| n >= 1)
                            .map(|n| (Anchor::MarkIndex(n), 3))
                    } else {
                        None
                    }
                } else if word(text) && continues(self.pos + 3) {
                    Some((Anchor::Mark(text.clone()), 3))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Consume the rounded anchor [`Self::peek_anchor`] saw, if
    /// any; leaves the position untouched otherwise.
    fn mark_anchor(&mut self) -> Option<Anchor> {
        if matches!(self.peek(), Some(Token::IdAnchorOpen)) {
            return self.id_anchor().ok();
        }
        let (anchor, len) = self.peek_anchor()?;
        self.pos += len;
        Some(anchor)
    }

    /// `((!expr))` — the node-id anchor: the opener, an expression
    /// yielding the id, the two closing parens.
    fn id_anchor(&mut self) -> Result<Anchor> {
        self.pos += 1;
        let op = self.additive()?;
        for _ in 0..2 {
            if !matches!(self.peek(), Some(Token::RParen)) {
                return Err(QuarbError::parse(
                    "expected '))' to close the node-id anchor '((!…))'",
                ));
            }
            self.pos += 1;
        }
        Ok(Anchor::Id(Box::new(op)))
    }

    /// The argument list of a `%(...)` / `%%(...)` record form —
    /// the record convention, normalized to the `rec` call the
    /// sigil is canonical for.
    fn record_args(&mut self) -> Result<FnCall> {
        self.expect(Token::LParen, "'(' after '%'")?;
        let mut args = Vec::new();
        if !matches!(self.peek(), Some(Token::RParen)) {
            loop {
                self.record_item(&mut args)?;
                if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen, "')' to close the record")?;
        Ok(FnCall {
            name: "rec".to_string(),
            args,
        })
    }

    fn func_call(&mut self) -> Result<FnCall> {
        let name = match self.bump() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => text.clone(),
            _ => {
                return Err(QuarbError::parse("expected a command name after '|'"));
            }
        };
        // `ALIAS:name` — an environment command: the mount alias as
        // a prefix glued by a colon (the colon lexes as the field
        // token before a glued word).
        let name = match (self.peek(), self.toks.get(self.pos + 1)) {
            (
                Some(Token::Field | Token::Colon { spaced: false }),
                Some(Token::Name {
                    text,
                    quoted: false,
                    glued: true,
                }),
            ) if is_word(&name) => {
                let full = format!("{name}:{text}");
                self.pos += 2;
                full
            }
            _ => name,
        };
        let environment = crate::env::split_env(&name).is_some();
        if !environment
            && let Some(pointer) = retired_command(&name)
        {
            return Err(QuarbError::parse(pointer));
        }
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            // The record-convention calls (`rec`, `record`, `group`)
            // take `key = value` items and `;` between them as the
            // sigil forms do.
            let convention = is_record_convention(&name);
            // The parentheses hold the command's operands and options: a bare
            // word among them is a string, not a command (the
            // record-convention calls take fields, which are
            // bodies like the sigil form's).
            let saved_depth = self.arg_depth;
            if !convention {
                self.arg_depth += 1;
            }
            let parsed = self.func_args(&name, convention, &mut args);
            self.arg_depth = saved_depth;
            parsed?;
            self.expect(Token::RParen, "')' to close the command's operands")?;
        }
        self.finish_call(name, args)
    }

    /// The argument list of a call, up to (not including) its
    /// closing parenthesis.
    fn func_args(&mut self, name: &str, convention: bool, args: &mut Vec<Arg>) -> Result<()> {
        {
            if !matches!(self.peek(), Some(Token::RParen)) {
                loop {
                    // The option segment: the last `;`-field, opened
                    // by a dash-led word, `--` or the void `-=`; it
                    // runs to the closing parenthesis.
                    if self.option_ahead() {
                        self.option_segment(name, args)?;
                        break;
                    }
                    if convention {
                        self.record_item(args)?;
                    } else if name == "csv"
                        && args.is_empty()
                        && let Some(Token::Name {
                            text, quoted: true, ..
                        }) = self.peek()
                    {
                        // `csv(";"; year; title)`: a quoted first
                        // argument is the delimiter; bare words are
                        // columns. Carried as `delimiter=X`.
                        let delimiter = match text.as_str() {
                            "comma" => ",".to_string(),
                            "semicolon" => ";".to_string(),
                            "tab" => "\t".to_string(),
                            "pipe" => "|".to_string(),
                            t if t.chars().count() == 1 && t.is_ascii() => t.to_string(),
                            _ => {
                                return Err(QuarbError::parse(format!(
                                    "csv takes one delimiter first: csv(\";\"), csv(\"tab\") \
                                     — not {text:?}"
                                )));
                            }
                        };
                        args.push(Arg::Named {
                            name: "delimiter".into(),
                            value: Value::Str(delimiter.to_string()),
                        });
                        self.pos += 1;
                    } else {
                        args.push(self.func_arg()?);
                    }
                    // `;` is the argument separator; `,` is sugar
                    // (spaced, so the decimal comma stays a number).
                    if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether an option opens here: a dash-led word (`-i`, `-n`),
    /// `--` glued to a word (`--locale`), or the void option `-=`.
    /// A dash before a digit, `$` or a sigil stays minus.
    fn option_ahead(&self) -> bool {
        match self.peek() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                lexer::is_option_word(text)
                    || (text == "-" && matches!(self.toks.get(self.pos + 1), Some(Token::OptEq)))
            }
            Some(Token::DashDash) => matches!(
                self.toks.get(self.pos + 1),
                Some(Token::Name {
                    quoted: false,
                    glued: true,
                    ..
                }) | Some(Token::OptEq)
            ),
            _ => false,
        }
    }

    /// The option segment of a simple command: whitespace-separated
    /// options in shell spelling, `=` binding a value (read as an
    /// operand reads: a bare word is a string). Once entered, every
    /// dash token is an option (`-9`); a `;` after it is refused,
    /// since the options stand last.
    fn option_segment(&mut self, command: &str, args: &mut Vec<Arg>) -> Result<()> {
        let saved_depth = self.arg_depth;
        self.arg_depth += 1;
        let parsed = self.option_items(command, args);
        self.arg_depth = saved_depth;
        parsed
    }

    fn option_items(&mut self, command: &str, args: &mut Vec<Arg>) -> Result<()> {
        loop {
            let name = match self.peek() {
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text.starts_with('-') => {
                    let t = text.clone();
                    self.pos += 1;
                    t
                }
                Some(Token::DashDash) => {
                    self.pos += 1;
                    match self.peek() {
                        Some(Token::Name {
                            text,
                            quoted: false,
                            glued: true,
                        }) => {
                            let t = format!("--{text}");
                            self.pos += 1;
                            t
                        }
                        _ => "--".to_string(),
                    }
                }
                Some(Token::RParen) => return Ok(()),
                Some(Token::Comma | Token::Semi) => {
                    return Err(QuarbError::parse(format!(
                        "the options of '{command}' stand last: write the operands before the \
                         option segment, '{command}(operand; …; -opt --opt=value)'"
                    )));
                }
                _ => {
                    return Err(QuarbError::parse(format!(
                        "in the option segment of '{command}' every token is an option spelled \
                         as in the shell ('-i', '--name=value'); an operand stands before the segment"
                    )));
                }
            };
            let value = if matches!(self.peek(), Some(Token::OptEq)) {
                self.pos += 1;
                // The void option `-=`: empty name, empty value; it
                // does nothing but open the segment.
                if name == "-" {
                    if matches!(self.peek(), Some(Token::Name { glued: true, .. })) {
                        return Err(QuarbError::parse(format!(
                            "the void option '-=' of '{command}' takes no value; it opens the \
                             segment for a numeric flag ('-= -9')"
                        )));
                    }
                    continue;
                }
                // One value, as an operand reads (a bare word a
                // string, a sigil an expression); the options are
                // whitespace-separated, so it ends at the space.
                Some(self.unary()?)
            } else {
                None
            };
            if name == "-" {
                return Err(QuarbError::parse(format!(
                    "a lone '-' in the option segment of '{command}' is the void option and is \
                     written '-='; a numeric flag follows it ('-= -9')"
                )));
            }
            args.push(Arg::Opt { name, value });
        }
    }

    /// A parsed call's shape checks.
    fn finish_call(&mut self, name: String, args: Vec<Arg>) -> Result<FnCall> {
        // The options a command declares, as its synopsis lists
        // them; every other one is refused by name. An environment
        // command's options are the callee's to judge.
        let declared: &[&str] = crate::registry::spec(&name).map_or(&[], |s| s.options);
        for arg in &args {
            if let Arg::Opt { name: opt, .. } = arg
                && crate::env::split_env(&name).is_none()
                && !declared.contains(&opt.as_str())
            {
                return Err(QuarbError::parse(if declared.is_empty() {
                    format!("'{name}' takes no options; its parentheses hold operands ('{opt}' given)")
                } else {
                    format!(
                        "'{name}' has no option '{opt}' (its options: {})",
                        declared.join(" ")
                    )
                }));
            }
        }
        // A range argument is `window`'s span, `substr`'s, or a rank
        // range on the commands that select by rank.
        if !matches!(
            name.as_str(),
            "window" | "substr" | "frequencies" | "top" | "bottom" | "first" | "last"
        ) && args.iter().any(|a| matches!(a, Arg::Range(_, _)))
        {
            return Err(QuarbError::parse(format!(
                "'{name}' takes no range argument ('window(a..b)' does)"
            )));
        }
        let call = FnCall { name, args };
        validate_stat_args(&call)?;
        Ok(call)
    }

    /// One command operand: a full value expression; a plain
    /// literal stays a literal. An offset range (`window(-2..0)`,
    /// either end optional) lexes as a single name token — digits,
    /// `-`, and `.` are all name characters — like the positional
    /// range predicate.
    /// One item of the record convention: `key = value`
    /// — a bare or quoted name followed by `=` names the value after
    /// it — or a value alone (auto-named, or named by a preceding
    /// literal in the flat `k, v` list form, the Perl-list heritage).
    fn record_item(&mut self, args: &mut Vec<Arg>) -> Result<()> {
        if let Some(Token::Name { text, .. }) = self.peek()
            && matches!(self.toks.get(self.pos + 1), Some(Token::Eq))
        {
            let key = text.clone();
            self.pos += 2;
            args.push(Arg::Lit(Value::Str(key)));
        }
        args.push(self.func_arg()?);
        Ok(())
    }

    fn func_arg(&mut self) -> Result<Arg> {
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && let Some((a, b)) = text.split_once("..")
        {
            let start = if a.is_empty() { None } else { a.parse().ok() };
            let end = if b.is_empty() { None } else { b.parse().ok() };
            if (a.is_empty() || start.is_some()) && (b.is_empty() || end.is_some()) {
                self.pos += 1;
                return Ok(Arg::Range(start, end));
            }
        }
        // Function parentheses already delimit each argument, so a
        // conditional argument needs no second pair:
        // `rec("age", ::Age ? ::Age * 1 : 1912 - $.born)`.
        match self.cond_expr()? {
            Operand::Lit(v) => Ok(Arg::Lit(v)),
            // `@("a"; "b")` of literals: the list as one literal
            // (`jaccard(@("cat"; "dog"))`).
            Operand::List(items) if items.iter().all(|i| matches!(i, Operand::Lit(_))) => {
                Ok(Arg::Lit(Value::list(
                    items
                        .into_iter()
                        .map(|i| match i {
                            Operand::Lit(v) => v,
                            _ => unreachable!(),
                        })
                        .collect(),
                )))
            }
            expr => Ok(Arg::Expr(expr)),
        }
    }

    /// Parse an optional trailing projection (`::`, `:::`, `::::`).
    fn projection(&mut self) -> Result<Option<Projection>> {
        let proj = match self.peek() {
            Some(Token::ColonColon) => {
                self.pos += 1;
                Projection::Property(self.opt_projection_name())
            }
            Some(Token::ColonColonColon) => {
                self.pos += 1;
                Projection::CoreMeta(self.require_projection_name("core metadata `:::`")?)
            }
            Some(Token::SemiSemiSemi) => {
                self.pos += 1;
                Projection::AdapterMeta(self.require_projection_name("adapter metadata `::::`")?)
            }
            _ => return Ok(None),
        };
        Ok(Some(proj))
    }

    fn opt_projection_name(&mut self) -> Option<String> {
        if let Some(Token::Name {
            text,
            quoted,
            glued,
        }) = self.peek()
        {
            // A projection's key is written glued to its `::`
            // (`::price`); a spaced name is not the key but whatever
            // follows the bare projection — an arithmetic operator
            // (`/price:: * /qty::`), a keyword, a literal.
            if !glued {
                return None;
            }
            // `and`/`or`/`not` are predicate keywords; unquoted, they are
            // not property names, so a bare `::` before one is the
            // default projection (e.g. `$*1/id:: and …`). A field with
            // one of these names must be quoted (`::'and'`).
            if !quoted && is_bool_word(text) {
                return None;
            }
            let name = text.clone();
            self.pos += 1;
            Some(name)
        } else {
            None
        }
    }

    fn require_projection_name(&mut self, what: &str) -> Result<String> {
        self.opt_projection_name()
            .ok_or_else(|| QuarbError::parse(format!("{what} needs a key")))
    }

    /// Parse one path element: a resolution step, a path-pattern
    /// group (`(...)` in strict form), or a plain step. A nav-op
    /// directly before `(` is the tolerated group form (the op
    /// left-distributes over name-only alternatives); a nav-op or a
    /// named hop directly before a brace quantifier is sugar for a
    /// one-element group (`/{2}` ≡ `(/.){2}`, `/div{2}` ≡
    /// `(/div){2}`).
    fn path_elem(&mut self) -> Result<PathElem> {
        self.descend()?;
        let r = self.path_elem_inner();
        self.nest_depth -= 1;
        r
    }

    fn path_elem_inner(&mut self) -> Result<PathElem> {
        // Bare `.name` (no body) in path position marks the current
        // node — the context-typed push: nodes go to the mark
        // store, never the register. (`.name(` is a pattern push;
        // the group loop dispatches it before we get here.)
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && text.starts_with('.')
            && !text[1..].starts_with('.')
            && (self.pattern_depth == 0
                || !matches!(self.toks.get(self.pos + 1), Some(Token::LParen)))
        {
            // Bare `.` marks anonymously — the slot is still
            // `((.N))`-addressable, and `$$.`/`((@))` see it. A
            // mark name is an identifier or a quoted string glued
            // to the dot, like every push name.
            let name = if text == "." {
                match self.toks.get(self.pos + 1) {
                    Some(Token::Name {
                        text: q,
                        quoted: true,
                        glued: true,
                    }) => {
                        let q = q.clone();
                        self.pos += 1;
                        Some(q)
                    }
                    _ => None,
                }
            } else {
                let n = &text[1..];
                if !n.chars().next().is_some_and(|c| c.is_alphabetic()) {
                    return Err(QuarbError::parse(format!(
                        "a mark name starts with a letter ('.name'); quote any other \
                         ('.\"{n}\"'), and recall a position with ((.N))"
                    )));
                }
                Some(n.to_string())
            };
            self.pos += 1;
            return Ok(PathElem::Mark(name));
        }
        if self.is_resolution_ahead() {
            return Ok(PathElem::Step(self.resolution_step()?));
        }
        if matches!(self.peek(), Some(Token::Resolve | Token::ReverseResolve)) {
            return Ok(PathElem::Step(self.bare_resolution_step()?));
        }
        if matches!(self.peek(), Some(Token::LParen)) {
            return Ok(PathElem::Group(self.group(None)?));
        }
        let axis = self.axis()?;
        if matches!(self.peek(), Some(Token::LParen)) {
            return Ok(PathElem::Group(self.group(Some(axis))?));
        }
        if matches!(self.peek(), Some(Token::Quant { .. })) {
            // Bare-operator sugar: only a single-hop operator
            // quantifies on its own (`//{2}` already means "any
            // depth" and refuses).
            if !matches!(
                axis,
                Axis::Child
                    | Axis::Parent
                    | Axis::NextSibling
                    | Axis::PrevSibling
                    | Axis::OutLink
                    | Axis::InLink
            ) {
                return Err(QuarbError::parse(
                    "a quantifier attaches to a single-hop operator or a \
                     parenthesized group",
                ));
            }
            let hop = Step {
                axis,
                matcher: Matcher::Dot,
                traits: Vec::new(),
                predicates: Vec::new(),
                leaf: false,
            };
            let quant = self.group_quant()?.expect("peeked a quantifier");
            let predicates = self.group_predicates()?;
            return Ok(PathElem::Group(Group {
                alts: vec![vec![PathElem::Step(hop)]],
                quant,
                predicates,
                traits: Vec::new(),
                reach: self.reach(),
            }));
        }
        let matcher = self.matcher_after(&axis)?;
        let step = self.finish_step(axis, matcher)?;
        // A brace quantifier after a named hop wraps it
        // (`/div{2}` ≡ `(/div){2}`); `+`/`*` are name characters, so
        // those two spellings need the parentheses.
        if matches!(self.peek(), Some(Token::Quant { .. })) {
            let quant = self.group_quant()?.expect("peeked a quantifier");
            let predicates = self.group_predicates()?;
            return Ok(PathElem::Group(Group {
                alts: vec![vec![PathElem::Step(step)]],
                quant,
                predicates,
                traits: Vec::new(),
                reach: self.reach(),
            }));
        }
        Ok(PathElem::Step(step))
    }

    /// Parse a path-pattern group after its opening `(`, through the
    /// closing `)` and any quantifier + reach suffix. With a
    /// `pending` axis (the tolerated form `/(p|div)`), each
    /// alternative must start with a bare name for the axis to
    /// distribute over; mixing with strict alternatives refuses.
    fn group(&mut self, pending: Option<Axis>) -> Result<Group> {
        self.pos += 1; // consume '('
        self.pattern_depth += 1;
        let mut alts = Vec::new();
        let alts_result = loop {
            match self.group_alt(&pending) {
                Ok(alt) => alts.push(alt),
                Err(e) => break Err(e),
            }
            match self.peek() {
                Some(Token::Pipe) => self.pos += 1,
                Some(Token::RParen) => {
                    self.pos += 1;
                    break Ok(());
                }
                _ => {
                    break Err(QuarbError::parse(
                        "expected '|' or ')' in a path-pattern group",
                    ));
                }
            }
        };
        self.pattern_depth -= 1;
        alts_result?;
        let quant = self.group_quant()?.unwrap_or(Quant {
            min: 1,
            max: Some(1),
        });
        // `<trait>` after a quantified group filters its matches
        // like a step's trait does, in the step's order (traits,
        // then predicates): `(>token){2}<word>[::lower != "the"]`.
        let traits = self.group_traits()?;
        let predicates = self.group_predicates()?;
        let reach = self.reach();
        Ok(Group {
            alts,
            quant,
            predicates,
            traits,
            reach,
        })
    }

    /// Parse the `<...>` trait clauses of a group (after the
    /// quantifier, before its predicates), as a step's.
    fn group_traits(&mut self) -> Result<Vec<TraitClause>> {
        let mut traits = Vec::new();
        while let Some(clauses) = self.try_trait()? {
            traits.extend(clauses);
        }
        Ok(traits)
    }

    /// Parse the `[...]` predicates of a group (between the
    /// quantifier and the reach suffix). Expression predicates
    /// only: a positional predicate has no defined ordering across
    /// repetition tiers.
    fn group_predicates(&mut self) -> Result<Vec<Predicate>> {
        let mut predicates = Vec::new();
        while matches!(self.peek(), Some(Token::LBracket)) {
            match self.predicate()? {
                p @ Predicate::Expr(_) => predicates.push(p),
                _ => {
                    return Err(QuarbError::parse(
                        "a group takes expression predicates only \
                         (positional selection has no order across \
                         repetition tiers)",
                    ));
                }
            }
        }
        Ok(predicates)
    }

    /// One alternative of a path-pattern group: a non-empty element
    /// sequence, ended by `|` or `)`.
    fn group_alt(&mut self, pending: &Option<Axis>) -> Result<Vec<PathElem>> {
        let mut elems = Vec::new();
        if let Some(axis) = pending {
            // Tolerated form: the axis before '(' distributes over a
            // leading bare name (`/(p|div)` ≡ `(/p|/div)`).
            match self.peek() {
                Some(Token::Name { .. } | Token::Regex(_)) => {
                    let matcher = self.matcher_after(&axis)?;
                    elems.push(PathElem::Step(self.finish_step(axis.clone(), matcher)?));
                }
                _ => {
                    return Err(QuarbError::parse(
                        "the operator before '(' distributes over name \
                         alternatives; write the strict form '(/p|/div)'",
                    ));
                }
            }
        }
        loop {
            match self.peek() {
                Some(Token::Pipe | Token::RParen) | None => break,
                // A fragment invocation splices into the alternative.
                Some(Token::Amp) => {
                    self.splice_path_fragment(&mut elems, SplicePos::GroupAlt)?;
                }
                // `.(body)` / `.name(body)` — a breadcrumb pushed as
                // the path walks.
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text.starts_with('.')
                    && matches!(self.toks.get(self.pos + 1), Some(Token::LParen)) =>
                {
                    elems.push(self.pattern_push()?);
                }
                _ => elems.push(self.path_elem()?),
            }
        }
        if !elems
            .iter()
            .any(|e| matches!(e, PathElem::Step(_) | PathElem::Group(_)))
        {
            return Err(QuarbError::parse(
                "a path-pattern alternative needs at least one hop",
            ));
        }
        Ok(elems)
    }

    /// Parse a pattern push `.(body)` / `.name(body)` — the same
    /// query-then-value-expression fallback as a pipeline
    /// subcontext.
    fn pattern_push(&mut self) -> Result<PathElem> {
        let name = match self.bump() {
            Some(Token::Name { text, .. }) => {
                let rest = &text[1..];
                if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_string())
                }
            }
            _ => unreachable!("peeked a dot-leading name"),
        };
        self.pos += 1; // consume '('
        // The body is not pattern content: `.` inside it is a
        // literal name again, and its own patterns re-open scope.
        let depth = std::mem::take(&mut self.pattern_depth);
        let save = self.pos;
        self.subquery_depth += 1;
        let tried = self.parse_query();
        self.subquery_depth -= 1;
        let body = if let Ok(q) = tried
            && matches!(self.peek(), Some(Token::RParen))
        {
            PushBody::Query(Box::new(q))
        } else {
            self.pos = save;
            match self.additive() {
                Ok(expr) => PushBody::Expr(expr),
                Err(e) => {
                    self.pattern_depth = depth;
                    return Err(e);
                }
            }
        };
        self.pattern_depth = depth;
        self.expect(Token::RParen, "')' to close a pattern push")?;
        Ok(PathElem::Push { name, body })
    }

    /// Consume an optional repetition quantifier after a group: a
    /// brace form (`{m,n}` — spacing free), or a glued `+` / `*`
    /// (name characters, so a spaced one is arithmetic, not a
    /// quantifier). Returns `None` when no quantifier is present.
    fn group_quant(&mut self) -> Result<Option<Quant>> {
        match self.peek() {
            Some(Token::Quant { min, max }) => {
                let (min, max) = (*min, *max);
                if max.is_some_and(|n| n < min) {
                    return Err(QuarbError::parse(format!(
                        "quantifier {{{min},{}}} has max below min",
                        max.expect("checked")
                    )));
                }
                self.pos += 1;
                Ok(Some(Quant { min, max }))
            }
            Some(Token::Name {
                text,
                quoted: false,
                glued: true,
            }) if text == "+" => {
                self.pos += 1;
                Ok(Some(Quant { min: 1, max: None }))
            }
            Some(Token::Name {
                text,
                quoted: false,
                glued: true,
            }) if text == "*" => {
                self.pos += 1;
                Ok(Some(Quant { min: 0, max: None }))
            }
            _ => Ok(None),
        }
    }

    /// Parse a step's tail (traits, predicates, leaf anchor) after its
    /// axis and matcher.
    fn finish_step(&mut self, axis: Axis, matcher: Matcher) -> Result<Step> {
        let mut traits = Vec::new();
        while let Some(clauses) = self.try_trait()? {
            traits.extend(clauses);
        }
        let mut predicates = Vec::new();
        while matches!(self.peek(), Some(Token::LBracket)) {
            predicates.push(self.predicate()?);
        }
        // A trailing `$` anchors the step to leaf nodes — but at
        // pipeline level a `$` glued to a pipe is the map pipe `$|`,
        // a command the caller must see; consuming its `$` here would
        // silently reparse `tags $| upper` as `tags$ | upper` (leaf
        // anchor + plain pipe), dropping the map semantics. Inside a
        // path-pattern group (`pattern_depth > 0`) a following `|` is
        // the alternation separator instead, so the `$` there really
        // is a leaf anchor and must still be consumed.
        let map_pipe_ahead =
            self.pattern_depth == 0 && matches!(self.toks.get(self.pos + 1), Some(Token::Pipe));
        let leaf = if matches!(self.peek(), Some(Token::Dollar)) && !map_pipe_ahead {
            self.pos += 1;
            true
        } else {
            false
        };
        Ok(Step {
            axis,
            matcher,
            traits,
            predicates,
            leaf,
        })
    }

    /// Whether the next tokens are `::name-->` (a resolution step),
    /// `::name=>` (a value match), or `::name<--` (the retired
    /// spelling, refused with its replacement).
    fn is_resolution_ahead(&self) -> bool {
        matches!(self.toks.get(self.pos), Some(Token::ColonColon))
            && matches!(self.toks.get(self.pos + 1), Some(Token::Name { .. }))
            && matches!(
                self.toks.get(self.pos + 2),
                Some(Token::Resolve | Token::ReverseResolve | Token::ValueMatch)
            )
    }

    /// Parse `::property-->label` (a resolution step; the name
    /// glued to the arrow is its reference label) or
    /// `::property=>(SCOPE::target)` (a value match). The retired
    /// `::property<--` is refused with its replacement: the property
    /// of the referring nodes goes in the arrow's argument.
    fn resolution_step(&mut self) -> Result<Step> {
        self.pos += 1; // consume '::'
        let property = match self.bump() {
            Some(Token::Name { text, .. }) => text.clone(),
            _ => {
                return Err(QuarbError::parse(
                    "expected a property name before '-->' or '=>'",
                ));
            }
        };
        match self.peek() {
            Some(Token::ReverseResolve) => Err(QuarbError::parse(format!(
                "a property before '<--' has no meaning: the property is the \
                 referring nodes' own and goes in the argument — write \
                 '<--(::{property})', or '<--(/scope/*::{property})' to bound \
                 the search"
            ))),
            Some(Token::ValueMatch) => {
                self.pos += 1;
                let (scope, target) = self.match_argument()?;
                if scope.is_none() && self.predicate_depth > 0 {
                    return Err(QuarbError::parse(
                        "an unscoped value match '=>(::b)' is not allowed inside a \
                         predicate (it would walk the whole arbor per node); give \
                         it a scope, '=>(/path/*::b)'",
                    ));
                }
                self.finish_step(
                    Axis::ValueMatch {
                        property,
                        scope,
                        target,
                    },
                    Matcher::Any,
                )
            }
            _ => self.resolution_tail(Some(property)),
        }
    }

    /// The bare arrow — `//a-->` with no property named: each node's
    /// own reference property (an html anchor's `href`) resolves, as
    /// the adapter declares it; or `<--`, the reverse arrow with its
    /// own forms.
    fn bare_resolution_step(&mut self) -> Result<Step> {
        if matches!(self.peek(), Some(Token::ReverseResolve)) {
            return self.reverse_step();
        }
        self.resolution_tail(None)
    }

    /// `-->hint`: the forward arrow's relation hint, then the step's
    /// tests.
    fn resolution_tail(&mut self, property: Option<String>) -> Result<Step> {
        self.pos += 1; // the arrow
        let hint = self.bare_hint();
        self.finish_step(
            Axis::Resolve {
                property,
                label: hint,
            },
            Matcher::Any,
        )
    }

    /// An optional relation hint: a bare name glued after an arrow.
    fn bare_hint(&mut self) -> Option<String> {
        match self.peek() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                let h = text.clone();
                self.pos += 1;
                Some(h)
            }
            _ => None,
        }
    }

    /// `<--`, `<--label`, `<--(ARG)`, `<--label(ARG)` — the reverse
    /// arrow. ARG is `::y`, `SCOPE::y`, `SCOPE::*` or `SCOPE`; the
    /// scope path is root-anchored. `::*` needs a scope; inside a
    /// predicate so does the arrow itself (the unscoped forms walk
    /// the whole arbor per node).
    fn reverse_step(&mut self) -> Result<Step> {
        self.pos += 1; // '<--'
        let hint = self.bare_hint();
        let (scope, property) = if matches!(self.peek(), Some(Token::LParen)) {
            let (scope, prop) = self.reverse_argument()?;
            (scope, prop)
        } else {
            (None, RefProp::Default)
        };
        if scope.is_none() && property == RefProp::Any {
            return Err(QuarbError::parse(
                "'<--(::*)' asks every property of every node: give it a scope, \
                 '<--(/path/*::*)'",
            ));
        }
        if scope.is_none() && self.predicate_depth > 0 {
            return Err(QuarbError::parse(
                "an unscoped reverse resolution '<--' is not allowed inside a \
                 predicate (it would walk the whole arbor per node); give it a \
                 scope, '<--(/path/*::y)', or use an incoming edge '<-'",
            ));
        }
        self.finish_step(
            Axis::ReverseResolve {
                label: hint,
                scope,
                property,
            },
            Matcher::Any,
        )
    }

    /// The parenthesized argument of the reverse arrow.
    fn reverse_argument(&mut self) -> Result<(Option<ScopePath>, RefProp)> {
        self.pos += 1; // '('
        let scope = self.scope_path()?;
        let property = if matches!(self.peek(), Some(Token::ColonColon)) {
            self.pos += 1;
            match self.bump() {
                Some(Token::Name { text, quoted, .. }) if text == "*" && !quoted => RefProp::Any,
                Some(Token::Name { text, .. }) => RefProp::Named(text.clone()),
                _ => {
                    return Err(QuarbError::parse(
                        "the reverse arrow's argument names a property after '::' \
                         ('<--(::y)', '<--(/scope/*::y)', '<--(/scope/*::*)')",
                    ));
                }
            }
        } else {
            RefProp::Default
        };
        if !matches!(self.bump(), Some(Token::RParen)) {
            return Err(QuarbError::parse(
                "expected ')' to close the reverse arrow's argument",
            ));
        }
        if scope.is_none() && property == RefProp::Default {
            return Err(QuarbError::parse(
                "'<--()' is empty: write '<--' for the whole arbor, '<--(::y)' \
                 for a property, or '<--(/scope/*)' for a scope",
            ));
        }
        Ok((scope, property))
    }

    /// The parenthesized argument of the value match: `(::b)` or
    /// `(SCOPE::b)`.
    fn match_argument(&mut self) -> Result<(Option<ScopePath>, String)> {
        if !matches!(self.bump(), Some(Token::LParen)) {
            return Err(QuarbError::parse(
                "the value match takes its argument in parentheses: \
                 '::a=>(/scope/*::b)' or '::a=>(::b)'",
            ));
        }
        let scope = self.scope_path()?;
        if !matches!(self.bump(), Some(Token::ColonColon)) {
            return Err(QuarbError::parse(
                "the value match names the property to compare after '::': \
                 '::a=>(/scope/*::b)'",
            ));
        }
        let target = match self.bump() {
            Some(Token::Name { text, .. }) if text != "*" => text.clone(),
            _ => {
                return Err(QuarbError::parse(
                    "the value match compares one named property ('::b')",
                ));
            }
        };
        if !matches!(self.bump(), Some(Token::RParen)) {
            return Err(QuarbError::parse(
                "expected ')' to close the value match's argument",
            ));
        }
        Ok((scope, target))
    }

    /// A root-anchored scope path inside an arrow's argument: the
    /// steps up to the `::` or the closing parenthesis; `None` when
    /// the argument opens with `::`. A leading `^` is accepted and
    /// means what it means.
    fn scope_path(&mut self) -> Result<Option<ScopePath>> {
        if matches!(self.peek(), Some(Token::ColonColon)) {
            return Ok(None);
        }
        if matches!(self.peek(), Some(Token::Caret)) {
            self.pos += 1;
        }
        let mut steps = Vec::new();
        while matches!(
            self.peek(),
            Some(
                Token::Slash
                    | Token::SlashSlash
                    | Token::Backslash
                    | Token::BackslashBackslash
                    | Token::FollowingSiblings(_)
                    | Token::PrecedingSiblings(_)
                    | Token::NextSibling
                    | Token::PrevSibling
                    | Token::ArrowOut
                    | Token::ArrowIn
                    | Token::DashDash
                    | Token::LParen
            )
        ) {
            steps.push(self.path_elem()?);
        }
        if steps.is_empty() {
            return Err(QuarbError::parse(
                "the arrow's argument opens with a scope path ('/books/*', '//a') \
                 or with '::'",
            ));
        }
        Ok(Some(ScopePath(steps)))
    }

    /// Parse one `[...]` predicate: an index `[n]` / `[-n]`, a range
    /// `[a..b]` (either end optional), or an expression.
    fn predicate(&mut self) -> Result<Predicate> {
        // A predicate's operand paths are not pattern content, even
        // on a step inside a group — `.` stays a literal name there
        // unless the operand opens a pattern of its own.
        let depth = std::mem::take(&mut self.pattern_depth);
        self.predicate_depth += 1;
        let result = self.predicate_inner();
        self.predicate_depth -= 1;
        self.pattern_depth = depth;
        result
    }

    fn predicate_inner(&mut self) -> Result<Predicate> {
        self.pos += 1; // consume '['
        // `[n]` / `[a..b]` — a lone bare number or range — is a
        // positional predicate. Both lex as a single name token
        // (digits, `-`, and `.` are all name characters).
        if let (
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }),
            Some(Token::RBracket),
        ) = (self.toks.get(self.pos), self.toks.get(self.pos + 1))
        {
            if let Ok(n) = text.parse::<i64>() {
                self.pos += 2;
                return Ok(Predicate::Index(n));
            }
            // A lone digit run that failed the i64 parse is an
            // overflowing index — error, rather than falling through
            // to a float operand whose truthiness keeps every node.
            let digits = text.strip_prefix('-').unwrap_or(text);
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(QuarbError::parse(format!(
                    "positional index [{text}] is out of range"
                )));
            }
            if let Some((a, b)) = text.split_once("..") {
                let start = if a.is_empty() { None } else { a.parse().ok() };
                let end = if b.is_empty() { None } else { b.parse().ok() };
                // Both sides must be clean: absent, or an integer.
                // Anything else (e.g. `2..x`, `1.5..2`) is not a
                // positional range.
                if (a.is_empty() || start.is_some()) && (b.is_empty() || end.is_some()) {
                    self.pos += 2;
                    return Ok(Predicate::Range(start, end));
                }
            }
        }
        let expr = self.pred_or()?;
        self.expect(
            Token::RBracket,
            "']' to close a predicate (')' for the '(?' spelling)",
        )?;
        Ok(Predicate::Expr(expr))
    }

    /// A bare `and` / `or` / `not` where a connective may stand is
    /// refused with the pointer to the symbol and the dotted word.
    fn refuse_bare_bool_word(&self) -> Result<()> {
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && let Some((w, sym)) = BARE_BOOL_WORDS.iter().find(|(w, _)| w == text)
        {
            return Err(QuarbError::parse(format!(
                "the bare word '{w}' is retired: write '{sym}' (or the dotted '.{w}.')"
            )));
        }
        Ok(())
    }

    fn pred_or(&mut self) -> Result<PredExpr> {
        let mut left = self.pred_and()?;
        self.refuse_bare_bool_word()?;
        while self.eat_word(OR_WORDS)
            || matches!(self.peek(), Some(Token::PipePipe)) && {
                self.pos += 1;
                true
            }
        {
            let right = self.pred_and()?;
            self.refuse_bare_bool_word()?;
            left = PredExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn pred_and(&mut self) -> Result<PredExpr> {
        let mut left = self.pred_not()?;
        while self.eat_word(AND_WORDS)
            || matches!(self.peek(), Some(Token::AmpAmp)) && {
                self.pos += 1;
                true
            }
        {
            let right = self.pred_not()?;
            left = PredExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn pred_not(&mut self) -> Result<PredExpr> {
        self.refuse_bare_bool_word()?;
        self.descend()?;
        let r = self.pred_not_inner();
        self.nest_depth -= 1;
        r
    }

    fn pred_not_inner(&mut self) -> Result<PredExpr> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.pos += 1;
            return Ok(PredExpr::Not(Box::new(self.pred_not()?)));
        }
        if self.eat_word(NOT_WORDS) {
            return Ok(PredExpr::Not(Box::new(self.pred_not()?)));
        }
        self.pred_primary()
    }

    fn pred_primary(&mut self) -> Result<PredExpr> {
        let left = self.additive()?;
        if let Some(t @ (Token::Gt { spaced: false, .. } | Token::Lt { spaced: false, .. })) =
            self.peek()
        {
            let c = if matches!(t, Token::Gt { .. }) {
                '>'
            } else {
                '<'
            };
            return Err(QuarbError::parse(format!(
                "'{c}' glued to what follows is the sibling hop; the comparison \
                 is spaced: '::x {c} 3'"
            )));
        }
        // `::x> 3` — spaced on one side only: neither the hop nor
        // the comparison (spec: Lexical Disambiguation).
        if let Some(t @ (Token::Gt { before: false, .. } | Token::Lt { before: false, .. })) =
            self.peek()
        {
            let c = if matches!(t, Token::Gt { .. }) {
                '>'
            } else {
                '<'
            };
            return Err(QuarbError::parse(format!(
                "'{c}' is an operator and takes a space on both sides: write ' {c} ' \
                 (the sibling hop is glued to its name: '{c}name')"
            )));
        }
        if let Some(op) = self.cmp_op() {
            // `==` / `!==` are the pattern comparisons: a regex or
            // pattern literal rides them only. `=` / `!=` are
            // equality; a literal pattern after them is refused with
            // the pointer.
            match (op, self.peek()) {
                (CmpOp::Eq, Some(Token::Regex(_))) => {
                    return Err(QuarbError::parse(
                        "'= (/…/)' is retired: the pattern comparison is '== (/…/)'",
                    ));
                }
                (CmpOp::Ne, Some(Token::Regex(_))) => {
                    return Err(QuarbError::parse(
                        "'!= (/…/)' is retired: the pattern comparison is '!== (/…/)'",
                    ));
                }
                _ => {}
            }
            if matches!(op, CmpOp::Eq | CmpOp::Ne | CmpOp::Match | CmpOp::NotMatch)
                && let Some(right) =
                    self.pattern_operand(matches!(op, CmpOp::Match | CmpOp::NotMatch))?
            {
                if matches!(op, CmpOp::Eq | CmpOp::Ne) {
                    let eq = if matches!(op, CmpOp::Eq) { "==" } else { "!==" };
                    return Err(QuarbError::parse(format!(
                        "a pattern literal after '{}' is retired: the pattern comparison is '{eq}' ('{eq} {}')",
                        cmp_text(op),
                        crate::unparse::operand_text(&right)
                    )));
                }
                let op = match op {
                    CmpOp::Match => CmpOp::Eq,
                    CmpOp::NotMatch => CmpOp::Ne,
                    op => op,
                };
                return Ok(PredExpr::Compare(left, op, right));
            }
            let right = self.additive()?;
            Ok(PredExpr::Compare(left, op, right))
        } else {
            Ok(PredExpr::Truthy(left))
        }
    }

    /// Ruling #33 — a pattern literal after `=` / `!=`: a glued,
    /// strictly alternating chain of bare `*` and quoted strings —
    /// `*"sub"*` contains, `"app"*` prefix, `*".gz"` suffix,
    /// `*"a"*"b"*` multi-segment. Adjacency is the syntax: the
    /// first token may be spaced (it follows the operator), every
    /// later one must be glued — a spaced `*` is multiplication's
    /// spelling and never a pattern segment. Returns None (position
    /// restored) when the tokens aren't a pattern, so a plain
    /// string or arithmetic parses as before.
    ///
    /// After `==` / `!==` (`bare`), a quoted string with no star is
    /// a pattern too — the glob without a wildcard: literal text,
    /// anchored at both ends, as in the shell. It is not a regex.
    fn pattern_operand(&mut self, bare: bool) -> Result<Option<Operand>> {
        let start = self.pos;
        let star = |t: &Token| matches!(t, Token::Name { text, quoted: false, .. } if text == "*");
        let is_glued = |t: &Token| match t {
            Token::Name { glued, .. } => *glued,
            _ => false,
        };
        let mut segs: Vec<PatSeg> = Vec::new();
        loop {
            let Some(tok) = self.peek() else { break };
            let first = segs.is_empty();
            if !first && matches!(tok, Token::Interp(_)) {
                return Err(QuarbError::parse(
                    "a pattern segment must be a literal string — \
                     an interpolated \"${...}\" hole cannot glob; \
                     use == with a path for a dynamic pattern",
                ));
            }
            if !first && !is_glued(tok) {
                break;
            }
            match tok {
                t if star(t) => {
                    if matches!(segs.last(), Some(PatSeg::Star)) {
                        break;
                    }
                    segs.push(PatSeg::Star);
                }
                Token::Name {
                    text, quoted: true, ..
                } => {
                    if matches!(segs.last(), Some(PatSeg::Lit(_))) {
                        break;
                    }
                    segs.push(PatSeg::Lit(text.clone()));
                }
                _ => break,
            }
            self.pos += 1;
        }
        let stars = segs.iter().filter(|s| matches!(s, PatSeg::Star)).count();
        let lits = segs.len() - stars;
        if stars >= 1 && lits >= 1 {
            // A glued tail that is neither a star nor a string
            // (`*"a"*x`, `*"a"3`) is a malformed segment, not a
            // separate token — refuse rather than half-match.
            if let Some(t) = self.peek()
                && is_glued(t)
            {
                return Err(QuarbError::parse(
                    "a pattern segment must be a quoted string or the \
                     glob star",
                ));
            }
            return Ok(Some(Operand::Pattern(segs)));
        }
        if bare && stars == 0 && lits == 1 {
            // `== "x"`: only when the string is the whole operand —
            // a glued or arithmetic continuation is an expression.
            let continues = match self.peek() {
                Some(Token::Name {
                    glued,
                    text,
                    quoted: false,
                    ..
                }) => *glued || matches!(text.as_str(), "+" | "-" | "*" | "div" | "mod"),
                Some(Token::Name {
                    glued,
                    quoted: true,
                    ..
                }) => *glued,
                Some(Token::Interp(_)) => true,
                _ => false,
            };
            if !continues {
                return Ok(Some(Operand::Pattern(segs)));
            }
        }
        if stars == 1 && lits == 0 {
            // The bare `= *` (and the spaced `= * \"x\"`): a lone
            // star is not a pattern.
            return Err(QuarbError::parse(
                "a lone '*' is not a pattern — attach it to a string \
                 (*\"...\"*, \"...\"*), quote it ('*') to compare \
                 against the literal star, or test existence with the \
                 bare key",
            ));
        }
        self.pos = start;
        Ok(None)
    }

    /// A value expression at additive precedence: `term (+|- term)*`.
    /// The spaced `+` / `-` lex as lone name tokens (glued they are
    /// name characters), so the operator test is exact-text.
    fn additive(&mut self) -> Result<Operand> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "+" => ArithOp::Add,
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "-" => ArithOp::Sub,
                _ => {
                    self.one_sided_arith(&left)?;
                    break;
                }
            };
            self.pos += 1;
            let right = self.multiplicative()?;
            left = Operand::Arith {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// An arithmetic operator spaced on one side only, after a
    /// complete operand (spec: Lexical Disambiguation): `::v -1`
    /// lexes the sign into the literal, `::v- 1` the dash into the
    /// name. Neither is an operation; both get the pointer to the
    /// spaced spelling rather than a message about the stray
    /// literal.
    fn one_sided_arith(&self, left: &Operand) -> Result<()> {
        let Some(Token::Name {
            text,
            quoted: false,
            glued: false,
        }) = self.peek()
        else {
            return Ok(());
        };
        let numeric = |t: &str| t.starts_with(|c: char| c.is_ascii_digit() || c == '.');
        let mut chars = text.chars();
        let op = match (chars.next(), chars.as_str()) {
            (Some(c @ ('+' | '-' | '*')), rest) if numeric(rest) => c,
            (_, _) if numeric(text) => match crate::unparse::operand_text(left).chars().last() {
                Some(c @ ('+' | '-' | '*')) => c,
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        Err(QuarbError::parse(format!(
            "'{op}' is an operator and takes a space on both sides: write ' {op} '"
        )))
    }

    /// `unary ((*|div|idiv|mod) unary)*`. A bare `*` here follows a
    /// complete operand, so it is multiplication, not the wildcard.
    fn multiplicative(&mut self) -> Result<Operand> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "*" => ArithOp::Mul,
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "div" => ArithOp::Div,
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "idiv" => ArithOp::IDiv,
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "mod" => ArithOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let right = self.unary()?;
            left = Operand::Arith {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// Unary minus, a parenthesized group, or a plain operand. A
    /// parenthesized boolean expression in operand position is its
    /// truth value, so `(a or b) and c` still groups as before.
    fn unary(&mut self) -> Result<Operand> {
        self.descend()?;
        let r = self.unary_inner();
        self.nest_depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> Result<Operand> {
        // `base:name` — a record's field, chained for
        // nested records; a leading `:name` reads the topic's field.
        let mut o = if matches!(self.peek(), Some(Token::Field)) {
            Operand::Topic
        } else {
            self.unary_primary()?
        };
        // `X:.r` — the cross-capsa read: a register
        // accessor glued to a node path reads the register of the
        // capsa standing at the node. The node side must be a bare
        // node operand — a path, `_`, `$$N` — without a projection
        // (a projected path is a value, and a value has no capsa).
        if let Some(Token::Capsa(body)) = self.peek() {
            let body = body.clone();
            let reg = match body.as_str() {
                "." => RegRef::Top,
                "@." => RegRef::Whole,
                "%." => RegRef::Record,
                "%%." => RegRef::FullRecord,
                b => {
                    let rest = &b[1..];
                    match rest.parse::<usize>() {
                        Ok(n) => RegRef::Index(n),
                        Err(_) => RegRef::Named(rest.to_string()),
                    }
                }
            };
            let bare = |o: &Operand| {
                matches!(
                    o,
                    Operand::Rel {
                        projection: None,
                        ..
                    } | Operand::Ctx {
                        projection: None,
                        ..
                    }
                )
            };
            let ok = match &o {
                Operand::Outer(inner) => bare(inner),
                other => bare(other),
            };
            if !ok {
                return Err(QuarbError::parse(format!(
                    "':{body}' reads the register of the capsa standing at a node: write it after a node path (<-link:{body}, \\/*:{body})"
                )));
            }
            self.pos += 1;
            o = Operand::PeerReg {
                at: Box::new(o),
                reg,
            };
        }
        while matches!(self.peek(), Some(Token::Field)) {
            self.pos += 1;
            let name = match self.bump() {
                Some(Token::Name { text, .. }) => text.clone(),
                _ => return Err(QuarbError::parse("expected a field name after ':'")),
            };
            if let Operand::Rel {
                projection: None, ..
            } = &o
            {
                return Err(QuarbError::parse(format!(
                    "':{name}' reads a record's field; a node's property is '::{name}'"
                )));
            }
            o = Operand::Field {
                base: Box::new(o),
                name,
            };
        }
        // `value:::key` — a value's provenance (the core-metadata
        // rung after a register value or the topic). A node path
        // parsed its own `:::` as a projection already.
        while matches!(self.peek(), Some(Token::ColonColonColon)) {
            if !matches!(
                o,
                Operand::Recall(_)
                    | Operand::Topic
                    | Operand::Field { .. }
                    | Operand::ValueMeta { .. }
                    | Operand::Piped { .. }
                    | Operand::PeerReg { .. }
            ) {
                return Err(QuarbError::parse(
                    "':::key' after a value reads its provenance: write it after a register value ($.name), the topic ($_), or a record field",
                ));
            }
            self.pos += 1;
            let key = self.require_projection_name("value provenance `:::`")?;
            o = Operand::ValueMeta {
                base: Box::new(o),
                key,
            };
        }
        Ok(o)
    }

    fn unary_primary(&mut self) -> Result<Operand> {
        if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "-") {
            self.pos += 1;
            return Ok(Operand::Neg(Box::new(self.unary()?)));
        }
        if matches!(self.peek(), Some(Token::MarkOpen)) {
            // `$$.name` followed by a path continuation is the mark
            // anchor — dispatch before the group readings claim it.
            return self.operand();
        }
        if matches!(self.peek(), Some(Token::LParen)) {
            self.refuse_single_paren_anchor()?;
            // A `(` here may open a path-pattern group (`[(->ref)+]`)
            // or a boolean/value group (`(::a or ::b)`). Try the path
            // reading first and back off on failure — a bare `(path)`
            // parses both ways with the same meaning, so preferring
            // the path changes nothing observable.
            let start = self.pos;
            // A leading `:name` after the paren is the topic's field
            // (`(:t | ngrams(3))`), never a path: skip the path
            // reading, which would otherwise swallow the pipe.
            if !matches!(self.toks.get(self.pos + 1), Some(Token::Field)) {
                match self.rel_from_group() {
                    Ok(op) => return Ok(op),
                    Err(_) => self.pos = start,
                }
            }
            self.pos += 1;
            let inner = self.cond_expr()?;
            // An operand may carry a pipe tail inside its parens:
            // `(expr | f @| g)` — command semantics, mirrored.
            let commands = self.inline_pipe_tail()?;
            self.expect(Token::RParen, "')' to close a group")?;
            return Ok(if commands.is_empty() {
                inner
            } else {
                Operand::Piped {
                    expr: Box::new(inner),
                    commands,
                }
            });
        }
        self.operand()
    }

    /// A conditional-bearing expression: a predicate expression,
    /// optionally followed by `? then : else`. Both branches parse
    /// this same rule, so chains need no inner parens (right-
    /// associative, as in Perl). Without a `?`, a truthy operand
    /// unwraps to itself and a genuinely boolean expression stays
    /// a boolean group — the established paren-group rule.
    /// The pipe tail an expression may carry inside its own
    /// parentheses — a group's, or a push body's: `| f`, `@| g`,
    /// `$| h`, and fragment invocations, up to the closing paren.
    fn inline_pipe_tail(&mut self) -> Result<Vec<Command>> {
        let mut commands = Vec::new();
        loop {
            match self.peek() {
                Some(Token::Pipe) if matches!(self.toks.get(self.pos + 1), Some(Token::Amp)) => {
                    self.pos += 1;
                    self.invoke_inline_fragment("|", &mut commands)?;
                }
                Some(Token::Pipe) => {
                    self.pos += 1;
                    commands.push(self.inline_command()?);
                }
                Some(Token::At)
                    if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe))
                        && matches!(self.toks.get(self.pos + 2), Some(Token::Amp)) =>
                {
                    self.pos += 2;
                    self.invoke_inline_fragment("@|", &mut commands)?;
                }
                Some(Token::At) if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) => {
                    self.pos += 2;
                    commands.push(self.inline_agg_command()?);
                }
                Some(Token::Dollar) if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) => {
                    self.pos += 2;
                    commands.push(Command::Map(Box::new(self.map_command()?)));
                }
                _ => break,
            }
        }
        Ok(commands)
    }

    /// A headless body pipeline: `name` applied to the body's seed
    /// as `| name` applies to its input, with whatever pipe tail
    /// follows (`cc`, `unique @| count`).
    fn headless_body(&mut self, name: String) -> Result<Operand> {
        if crate::registry::spec(&name).is_none() && retired_command(&name).is_none() {
            return Err(QuarbError::parse(format!(
                "'{name}' is not a command; a bare word in a body names one: \
                 quote a string \"{name}\""
            )));
        }
        let call = FnCall {
            name,
            args: Vec::new(),
        };
        let mut commands = vec![func_command(call)?];
        commands.extend(self.inline_pipe_tail()?);
        Ok(Operand::Piped {
            expr: Box::new(Operand::Seed),
            commands,
        })
    }

    fn cond_expr(&mut self) -> Result<Operand> {
        let cond = self.pred_or()?;
        if matches!(self.peek(), Some(Token::QuestionEq)) {
            self.pos += 1;
            let PredExpr::Truthy(scrutinee) = cond else {
                return Err(QuarbError::parse(
                    "the value match compares a VALUE: '(x ?= k ? r : else)'                      — a boolean condition belongs to the plain conditional",
                ));
            };
            let mut arms = Vec::new();
            let other = loop {
                let (test, regex) = if let Some(Token::Regex(pat)) = self.peek() {
                    let pat = pat.clone();
                    self.pos += 1;
                    (Operand::Lit(Value::Str(pat)), true)
                } else {
                    (self.additive()?, false)
                };
                if !matches!(self.peek(), Some(Token::Question { .. })) {
                    // The first expression not followed by `?` is
                    // the else.
                    if regex {
                        return Err(QuarbError::parse(
                            "a value match needs a final else after the                              regex arm: '(x ?= ~(pat) ? r : else)'",
                        ));
                    }
                    break test;
                }
                self.binary_marker('?')?;
                let result = self.additive()?;
                self.expect_else("':' after a value-match arm")?;
                arms.push((test, regex, result));
            };
            if arms.is_empty() {
                return Err(QuarbError::parse(
                    "a value match needs at least one arm:                      '(x ?= k ? r : else)'",
                ));
            }
            return Ok(Operand::Match {
                scrutinee: Box::new(scrutinee),
                arms,
                other: Box::new(other),
            });
        }
        if matches!(self.peek(), Some(Token::Question { .. })) {
            self.binary_marker('?')?;
            let then = self.cond_expr()?;
            self.expect_else("':' between the conditional's branches")?;
            let other = self.cond_expr()?;
            return Ok(Operand::Cond {
                cond: Box::new(cond),
                then: Box::new(then),
                other: Box::new(other),
            });
        }
        Ok(match cond {
            PredExpr::Truthy(op) => op,
            other => Operand::Group(Box::new(other)),
        })
    }

    /// A fragment invocation inside an inline pipe: splice its
    /// commands, holding them to the inline rules — a pipe inside an
    /// expression transforms a value, so a spliced command may not
    /// push or navigate.
    fn invoke_inline_fragment(
        &mut self,
        pipe: &'static str,
        commands: &mut Vec<Command>,
    ) -> Result<()> {
        let before = commands.len();
        self.invoke_pipeline_fragment(pipe, commands)?;
        for command in &commands[before..] {
            if matches!(
                command,
                Command::Push(_)
                    | Command::ExprPush { .. }
                    | Command::RecordPush { .. }
                    | Command::FieldsPush
                    | Command::Subcontext { .. }
                    | Command::AggPush { .. }
                    | Command::Nav(_)
            ) {
                return Err(QuarbError::parse(
                    "a fragment spliced inside an expression pipe may \
                     not push or navigate (pushes belong to real \
                     capsae; use it as a pipeline command instead)",
                ));
            }
        }
        Ok(())
    }

    /// One `| ...` command of an inline pipe. The pipeline's own
    /// command parser, minus what needs a real capsa: pushes and
    /// subcontexts refuse.
    fn inline_command(&mut self) -> Result<Command> {
        // Expression pipe tails live on the scalar plane; a
        // path-shaped command errors in pipe_item before it can
        // become navigation.
        let command = self.pipe_item(PipeMode::Scalar)?;
        match &command {
            Command::Push(_)
            | Command::ExprPush { .. }
            | Command::RecordPush { .. }
            | Command::FieldsPush
            | Command::Subcontext { .. } => Err(QuarbError::parse(
                "a pipe inside an expression transforms a value; \
                     pushes belong to real capsae (use a command)",
            )),
            _ => Ok(command),
        }
    }

    /// The command a `$|` maps over the topic's elements: a
    /// positional predicate slices the list, an expression
    /// predicate filters its elements (`$_` = the element), and
    /// any per-value command transforms them. Pushes and subcontexts
    /// refuse, as in inline pipes.
    fn map_command(&mut self) -> Result<Command> {
        // Hops are per-thread; `$|` transforms elements of a list
        // topic in place.
        if self.nav_command_ahead() {
            return Err(QuarbError::parse(
                "navigation doesn't ride the map pipe; '$|' transforms \
                 the elements of a list topic — write '| /path' as its \
                 own command",
            ));
        }
        if matches!(self.peek(), Some(Token::LBracket)) {
            let pred = self.predicate()?;
            return Ok(match pred {
                Predicate::Expr(e) => Command::Filter(e),
                positional => Command::Select(positional),
            });
        }
        self.inline_command()
    }

    /// One `@| ...` command of an inline pipe: an aggregate call, or
    /// positional selection.
    fn inline_agg_command(&mut self) -> Result<Command> {
        // The same discipline as the top-level `@|`: positional
        // selection only in brackets, known aggregates only, shapes
        // validated — an unchecked inline form would parse queries
        // whose conditions the executor silently ignores.
        if matches!(self.peek(), Some(Token::LBracket)) {
            return match self.predicate()? {
                pred @ (Predicate::Index(_) | Predicate::Range(_, _)) => Ok(Command::Select(pred)),
                Predicate::Expr(_) => Err(QuarbError::parse(
                    "a condition filters per capsa; write '| [cond]' \
                     ('@| [n]' selects positionally)",
                )),
            };
        }
        match self.agg_sigil_command()? {
            Some(Command::AggPush { .. }) => {
                return Err(QuarbError::parse(
                    "a pipe inside an expression transforms a value; \
                     pushes belong to real capsae (use a command)",
                ));
            }
            Some(command) => return Ok(command),
            None => {}
        }
        let call = self.func_call()?;
        agg_command(call)
    }

    /// The sigil forms after `@|`: the aggregate push `.name(body)`,
    /// the aggregate record `%(…)` / `%%(…)`, and the once-read
    /// register accessors `%.`, `%%.`, `$.name`, `$.`, `$.N`, `@.`.
    /// `None` when a command name follows instead.
    fn agg_sigil_command(&mut self) -> Result<Option<Command>> {
        match self.peek() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if text.starts_with('.') => {
                let text = text.clone();
                self.pos += 1;
                let mut name = if text == "." {
                    None
                } else {
                    Some(text[1..].to_string())
                };
                if name.is_none()
                    && let Some(Token::Name {
                        text: q,
                        quoted: true,
                        glued: true,
                    }) = self.peek()
                {
                    name = Some(q.clone());
                    self.pos += 1;
                }
                if let Some(n) = &name
                    && text.len() > 1
                    && !n.chars().next().is_some_and(|c| c.is_alphabetic())
                {
                    return Err(QuarbError::parse(format!(
                        "a push name starts with a letter ('@| .name(body)'); quote any other \
                         ('@| .\"{n}\"(body)')"
                    )));
                }
                if !matches!(self.peek(), Some(Token::LParen)) {
                    return Err(QuarbError::parse(
                        "the aggregate push takes a body: '@| .name(body)' files the body's \
                         value on every capsa (a plain '| .name' files each topic)",
                    ));
                }
                self.pos += 1;
                let expr = self.cond_expr()?;
                let commands = self.inline_pipe_tail()?;
                let body = if commands.is_empty() {
                    expr
                } else {
                    Operand::Piped {
                        expr: Box::new(expr),
                        commands,
                    }
                };
                self.expect(Token::RParen, "')' to close the aggregate push's body")?;
                Ok(Some(Command::AggPush { name, body }))
            }
            Some(Token::Percent) => {
                self.pos += 1;
                let enriched = if matches!(self.peek(), Some(Token::Percent)) {
                    self.pos += 1;
                    true
                } else {
                    false
                };
                if matches!(self.peek(), Some(Token::LParen)) {
                    let call = self.record_args()?;
                    validate_record(&call)?;
                    return Ok(Some(Command::AggRecord { call, enriched }));
                }
                self.expect_dot(if enriched { "%%" } else { "%" })?;
                Ok(Some(Command::AggRecall(if enriched {
                    RegRef::FullRecord
                } else {
                    RegRef::Record
                })))
            }
            Some(Token::Dollar) => {
                let save = self.pos;
                match self.additive()? {
                    Operand::Recall(r) => Ok(Some(Command::AggRecall(r))),
                    _ => {
                        self.pos = save;
                        Err(QuarbError::parse(
                            "after '@|' a register accessor reads once ('@| $.name', '@| %.'); \
                             an expression over it belongs to '|' or to a body",
                        ))
                    }
                }
            }
            Some(Token::At)
                if matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, .. }) if text == ".") =>
            {
                self.pos += 2;
                Ok(Some(Command::AggRecall(RegRef::Whole)))
            }
            _ => Ok(None),
        }
    }

    /// An operand-position relative path that begins with a
    /// path-pattern group, continued like any `Rel` operand.
    fn rel_from_group(&mut self) -> Result<Operand> {
        let mut steps = vec![PathElem::Group(self.group(None)?)];
        loop {
            if self.is_resolution_ahead() {
                steps.push(self.path_elem()?);
                continue;
            }
            if matches!(
                self.peek(),
                Some(
                    Token::Slash
                        | Token::SlashSlash
                        | Token::Backslash
                        | Token::BackslashBackslash
                        | Token::FollowingSiblings(_)
                        | Token::PrecedingSiblings(_)
                        | Token::NextSibling
                        | Token::PrevSibling
                        | Token::ArrowOut
                        | Token::ArrowIn
                        | Token::DashDash
                        | Token::LParen
                )
            ) {
                steps.push(self.path_elem()?);
                continue;
            }
            break;
        }
        let projection = self.projection()?;
        Ok(Operand::Rel {
            steps,
            projection,
            anchor: Anchor::Current,
        })
    }

    /// Parse a comparison operand: a relative path/projection, or a
    /// literal.
    /// Parse one `${...}` hole's source, in the same fragment table
    /// and parameter scope. Ruling #34: the hole may carry a glued
    /// bash operator — `expr:?` / `expr:?message` (the strict hole)
    /// or `expr:-fallback` (the default hole) — or a pipe tail
    /// (`expr | command ...`); mixing them wants the piped side
    /// parenthesized.
    fn parse_hole(&mut self, src: &str) -> Result<InterpSeg> {
        if let Some((idx, which)) = top_level_bash_op(src) {
            if idx == 0 || src[..idx].ends_with(|c: char| c.is_whitespace()) {
                return Err(QuarbError::parse(format!(
                    "in '${{{src}}}': '{which}' glues to its expression \
                     (`${{expr{which}...}}`)"
                )));
            }
            let expr = self.hole_expr(&src[..idx], src, false)?;
            let tail = &src[idx + 2..];
            return Ok(if which == ":?" {
                let msg = tail.trim();
                InterpSeg::Strict(expr, (!msg.is_empty()).then(|| msg.to_string()))
            } else {
                // The default is a shell word, as `${var:-word}`
                // spells it: a bare word is text, not a command.
                InterpSeg::Default(expr, self.hole_expr(tail, src, true)?)
            });
        }
        let context = |e: QuarbError| QuarbError::parse(format!("in '${{{src}}}': {e}"));
        let tokens = lexer::lex(src).map_err(context)?;
        let mut p = self.child(&tokens, self.defs.clone())?;
        p.def_params = self.def_params.clone();
        let expr = p.additive().map_err(context)?;
        // A pipe tail: the operand-pipe commands, exactly
        // as a parenthesized `(expr | f @| g)` carries them.
        let mut commands = Vec::new();
        loop {
            match p.peek() {
                Some(Token::Pipe) if matches!(p.toks.get(p.pos + 1), Some(Token::Amp)) => {
                    p.pos += 1;
                    p.invoke_inline_fragment("|", &mut commands)
                        .map_err(context)?;
                }
                Some(Token::Pipe) => {
                    p.pos += 1;
                    commands.push(p.inline_command().map_err(context)?);
                }
                Some(Token::At)
                    if matches!(p.toks.get(p.pos + 1), Some(Token::Pipe))
                        && matches!(p.toks.get(p.pos + 2), Some(Token::Amp)) =>
                {
                    p.pos += 2;
                    p.invoke_inline_fragment("@|", &mut commands)
                        .map_err(context)?;
                }
                Some(Token::At) if matches!(p.toks.get(p.pos + 1), Some(Token::Pipe)) => {
                    p.pos += 2;
                    commands.push(p.inline_agg_command().map_err(context)?);
                }
                _ => break,
            }
        }
        if p.pos != tokens.len() {
            return Err(QuarbError::parse(format!(
                "in '${{{src}}}': an interpolation hole holds one value \
                 expression (with an optional pipe tail or glued ':?' / ':-')"
            )));
        }
        Ok(InterpSeg::Expr(if commands.is_empty() {
            expr
        } else {
            Operand::Piped {
                expr: Box::new(expr),
                commands,
            }
        }))
    }

    /// One side of a hole's bash operator: a value expression with
    /// no top-level pipe (the operator and a tail combine only with
    /// the piped side parenthesized).
    /// One side of a hole's bash operator. With `word`, a bare word
    /// reads as text (the shell's `${var:-word}`), as a command's
    /// option does.
    fn hole_expr(&mut self, part: &str, whole: &str, word: bool) -> Result<Operand> {
        let context = |e: QuarbError| QuarbError::parse(format!("in '${{{whole}}}': {e}"));
        let tokens = lexer::lex(part).map_err(context)?;
        let mut depth = 0i32;
        for t in &tokens {
            match t {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::Pipe if depth == 0 => {
                    return Err(QuarbError::parse(format!(
                        "in '${{{whole}}}': a bash operator and a pipe tail \
                         are one or the other in a hole (parenthesize the \
                         piped expression)"
                    )));
                }
                _ => {}
            }
        }
        let mut p = self.child(&tokens, self.defs.clone())?;
        p.def_params = self.def_params.clone();
        p.arg_depth = usize::from(word);
        let expr = p.additive().map_err(context)?;
        if p.pos != tokens.len() {
            return Err(QuarbError::parse(format!(
                "in '${{{whole}}}': each side of ':?' / ':-' holds one \
                 value expression"
            )));
        }
        Ok(expr)
    }

    /// A relative path operand that opens with a step (`/`, `//`,
    /// `\\`, a sibling hop, an arrow): steps as long as they come,
    /// then the projection.
    fn rel_operand_from_step(&mut self) -> Result<Operand> {
        self.rel_operand_continue(Vec::new())
    }

    fn rel_operand_continue(&mut self, mut steps: Vec<PathElem>) -> Result<Operand> {
        loop {
            if self.is_resolution_ahead() {
                steps.push(self.path_elem()?);
                continue;
            }
            if matches!(
                self.peek(),
                Some(
                    Token::Slash
                        | Token::SlashSlash
                        | Token::Backslash
                        | Token::BackslashBackslash
                        | Token::FollowingSiblings(_)
                        | Token::PrecedingSiblings(_)
                        | Token::NextSibling
                        | Token::PrevSibling
                        | Token::ArrowOut
                        | Token::ArrowIn
                        | Token::DashDash
                        | Token::ReverseResolve
                        | Token::LParen
                )
            ) {
                steps.push(self.path_elem()?);
                continue;
            }
            break;
        }
        let projection = self.projection()?;
        Ok(Operand::Rel {
            steps,
            projection,
            anchor: Anchor::Current,
        })
    }

    fn operand(&mut self) -> Result<Operand> {
        match self.peek() {
            // A rounded-anchor operand path — `(name)`, `(N)`,
            // `(.)`, `(@)`, `(@name)`, `()` — navigates from the
            // anchored node(s). Same lookahead as the branch form;
            // parenthesized expressions keep `(` otherwise. The
            // plural forms gather values from every matching mark
            // (existential, like any multi-valued operand).
            Some(Token::MarkOpen | Token::IdAnchorOpen) if self.mark_anchor_ahead() => {
                let anchor = self.mark_anchor().expect("lookahead hit");
                let mut steps = Vec::new();
                loop {
                    if self.is_resolution_ahead() {
                        steps.push(self.path_elem()?);
                        continue;
                    }
                    if matches!(
                        self.peek(),
                        Some(
                            Token::Slash
                                | Token::SlashSlash
                                | Token::Backslash
                                | Token::BackslashBackslash
                                | Token::FollowingSiblings(_)
                                | Token::PrecedingSiblings(_)
                                | Token::NextSibling
                                | Token::PrevSibling
                                | Token::ArrowOut
                                | Token::ArrowIn
                                | Token::DashDash
                                | Token::LParen
                        )
                    ) {
                        steps.push(self.path_elem()?);
                        continue;
                    }
                    break;
                }
                let projection = self.projection()?;
                Ok(Operand::Rel {
                    steps,
                    projection,
                    anchor,
                })
            }
            // `^` — a root-anchored operand path: navigate from the
            // arbor root rather than the current node, mirroring the
            // branch anchor. Comparisons stay existential, so
            // `[::x = ^/set/*::x]` reads "equals SOME of them".
            // `*//name` opens one too: the universal search as an
            // operand (`(*//p @| count)`), root-anchored as in a
            // branch head.
            Some(Token::Caret) | Some(Token::Name { quoted: false, .. })
                if matches!(self.peek(), Some(Token::Caret))
                    || (matches!(self.peek(), Some(Token::Name { text, .. }) if text == "*")
                        && matches!(self.toks.get(self.pos + 1), Some(Token::SlashSlash))) =>
            {
                let mut steps = Vec::new();
                if matches!(self.peek(), Some(Token::Caret)) {
                    self.pos += 1;
                } else {
                    // The `*` of `*//`: the head step follows.
                    self.pos += 1;
                    let mut elem = self.path_elem()?;
                    universalize(&mut elem)?;
                    steps.push(elem);
                }
                loop {
                    if self.is_resolution_ahead() {
                        steps.push(self.path_elem()?);
                        continue;
                    }
                    if matches!(
                        self.peek(),
                        Some(
                            Token::Slash
                                | Token::SlashSlash
                                | Token::Backslash
                                | Token::BackslashBackslash
                                | Token::FollowingSiblings(_)
                                | Token::PrecedingSiblings(_)
                                | Token::NextSibling
                                | Token::PrevSibling
                                | Token::ArrowOut
                                | Token::ArrowIn
                                | Token::DashDash
                                | Token::LParen
                        )
                    ) {
                        steps.push(self.path_elem()?);
                        continue;
                    }
                    break;
                }
                let projection = self.projection()?;
                if steps.is_empty() && projection.is_none() {
                    return Err(QuarbError::parse(
                        "'^' in operand position starts a root-anchored path;                          follow it with steps or a projection",
                    ));
                }
                Ok(Operand::Rel {
                    steps,
                    projection,
                    anchor: Anchor::Root,
                })
            }
            // A relative path operand. It may descend (`/`, `//`),
            // ascend (`\`, `\\`), step sideways (`>>`, `<<`, the
            // rounded `;-` / `-;`), or follow a crosslink (`->`,
            // `<-`, `--`), so a structural predicate can ask "has any
            // outgoing link?" with `[->*]` and a record can carry the
            // parent's name with `rec(dir, \*:::name)`. The bare
            // `>` / `<` glued to a name are the sibling hops here as
            // anywhere: spacing decides (a spaced `>` is the
            // comparison, and cannot open an operand).
            Some(Token::Gt { spaced: false, .. } | Token::Lt { spaced: false, .. }) => {
                let first = self.path_elem()?;
                self.rel_operand_continue(vec![first])
            }
            // `<--(/scope/*::y)` opens an operand path too: a scoped
            // reverse resolution as a structural test (`[<--(...)]`,
            // `(<--(...) @| count)`); the unscoped form is refused in
            // a predicate by the step parser.
            // The bare forward arrow likewise: `[-->]` is "refers
            // somewhere", `%(t = -->::title)` the target's title.
            Some(Token::ReverseResolve | Token::Resolve) => {
                let first = self.path_elem()?;
                self.rel_operand_continue(vec![first])
            }
            Some(
                Token::Slash
                | Token::SlashSlash
                | Token::Backslash
                | Token::BackslashBackslash
                | Token::FollowingSiblings(_)
                | Token::PrecedingSiblings(_)
                | Token::NextSibling
                | Token::PrevSibling
                | Token::ArrowOut
                | Token::ArrowIn
                | Token::DashDash,
            ) => self.rel_operand_from_step(),
            // A resolution chain in operand position: follow the
            // reference(s), then project (`::album_id-->::title`).
            Some(Token::ColonColon) if self.is_resolution_ahead() => {
                let mut steps = Vec::new();
                loop {
                    if self.is_resolution_ahead() {
                        steps.push(self.path_elem()?);
                        continue;
                    }
                    if matches!(
                        self.peek(),
                        Some(
                            Token::Slash
                                | Token::SlashSlash
                                | Token::Backslash
                                | Token::BackslashBackslash
                                | Token::FollowingSiblings(_)
                                | Token::PrecedingSiblings(_)
                                | Token::NextSibling
                                | Token::PrevSibling
                                | Token::ArrowOut
                                | Token::ArrowIn
                                | Token::DashDash
                                | Token::LParen
                        )
                    ) {
                        steps.push(self.path_elem()?);
                        continue;
                    }
                    break;
                }
                let projection = self.projection()?;
                Ok(Operand::Rel {
                    steps,
                    projection,
                    anchor: Anchor::Current,
                })
            }
            Some(Token::ColonColon | Token::ColonColonColon | Token::SemiSemiSemi) => {
                let projection = self.projection()?.expect("projection start");
                Ok(Operand::Rel {
                    steps: Vec::new(),
                    projection: Some(projection),
                    anchor: Anchor::Current,
                })
            }
            // A call operand: a command word glued to `(` — the
            // pipe–call duality, `f(x, args) ≡ (x | f(args))`, the
            // first argument riding as the topic. `now()` is the one
            // nullary call (the invocation instant). A bare name
            // stays a string literal; the word operators never reach
            // here (eaten at the predicate/arithmetic level).
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                && text.chars().all(|c| c.is_alphanumeric() || c == '_')
                && !text.chars().next().is_some_and(|c| c.is_ascii_digit()) =>
            {
                let call = self.func_call()?;
                if call.name == "now" {
                    if !call.args.is_empty() {
                        return Err(QuarbError::parse(
                            "now() takes no arguments (it is the invocation instant)",
                        ));
                    }
                    return Ok(Operand::Now);
                }
                // The record convention breaks the call-operand
                // duality: `rec("name", x)` reads name-first, but
                // `(x0 | rec(rest))` rides the first argument as the
                // topic — the field name becomes data and the record
                // silently re-keys. Refuse rather than re-key; the
                // pipe form spells the topic-riding meaning outright.
                if let Some(pointer) = retired_command(&call.name) {
                    return Err(QuarbError::parse(pointer));
                }
                if matches!(call.name.as_str(), "rec" | "record") {
                    return Err(QuarbError::parse(format!(
                        "'{n}(...)' as an operand would ride its first \
                         field as the topic ('{n}(x, ...)' is \
                         '(x | {n}(...))'), silently re-keying the \
                         record; spell the pipe form '(x | {n}(...))' \
                         explicitly if that is meant",
                        n = call.name
                    )));
                }
                let mut args = call.args.into_iter();
                let first = match args.next() {
                    Some(Arg::Lit(v)) => Operand::Lit(v),
                    Some(Arg::Expr(e)) => e,
                    Some(Arg::Range(..)) | Some(Arg::Named { .. }) | Some(Arg::Opt { .. }) => {
                        return Err(QuarbError::parse(format!(
                            "'{}(...)' as an operand cannot ride a range or an option as its topic",
                            call.name
                        )));
                    }
                    None => {
                        return Err(QuarbError::parse(format!(
                            "a call operand needs a first argument to ride as the topic \
                             ('{0}(x)' is '(x | {0})'); only now() is nullary",
                            call.name
                        )));
                    }
                };
                // The duality must hold at parse time too: validate
                // the command half exactly as `| f(rest)` would be —
                // otherwise `foo(::x)` parses here and its unparse
                // `(::x | foo)` fails its own reparse.
                let command_call = FnCall {
                    name: call.name,
                    args: args.collect(),
                };
                let command = func_command(command_call)?;
                Ok(Operand::Piped {
                    expr: Box::new(first),
                    commands: vec![command],
                })
            }
            Some(Token::Name { text, quoted, .. }) => {
                let value = literal_value(text, *quoted);
                // In a body a bare word names a command applied to
                // the body's seed (`.n(cc)`, `%(total = count)`);
                // an unknown one is refused with its name, never
                // read as text. A command's own parentheses are the one
                // exception (`stem(ru)`): there it is the short
                // spelling of a quoted string.
                if !*quoted && self.arg_depth == 0 && is_word(text) && matches!(value, Value::Str(_))
                {
                    let name = text.clone();
                    self.pos += 1;
                    return self.headless_body(name);
                }
                self.pos += 1;
                Ok(Operand::Lit(value))
            }
            // `"text ${expr} text"` — an interpolated string: each
            // hole's source is lexed and parsed as a full value
            // expression, in the same parameter scope.
            Some(Token::Interp(parts)) => {
                let parts = parts.clone();
                self.pos += 1;
                let mut segs = Vec::new();
                for part in parts {
                    match part {
                        lexer::InterpPart::Text(t) => segs.push(InterpSeg::Text(t)),
                        lexer::InterpPart::Hole(src) => {
                            segs.push(self.parse_hole(&src)?);
                        }
                    }
                }
                Ok(Operand::Interp(segs))
            }
            Some(Token::Regex(pat)) => {
                let value = Value::Str(pat.clone());
                self.pos += 1;
                Ok(Operand::Lit(value))
            }
            // `%+` — the named-captures record.
            Some(Token::PercentPlus) => {
                self.pos += 1;
                Ok(Operand::NamedCaptures)
            }
            // `%(a = 1; ::x)` — the record literal as a value: a
            // field of another record, an item of a list, an
            // expression head. It is built where it stands — the
            // topic and the current node are the enclosing ones — so
            // it is the pipe form `($_ | %(...))` without the pipe.
            Some(Token::Percent) if matches!(self.toks.get(self.pos + 1), Some(Token::LParen)) => {
                self.pos += 1;
                let call = self.record_args()?;
                validate_record(&call)?;
                Ok(Operand::Piped {
                    expr: Box::new(Operand::Topic),
                    commands: vec![Command::Func(call)],
                })
            }
            // `@-` — all arrived-by edges (bare: labels; projected:
            // each edge's property); `@.` — the whole register;
            // `@(…)` — the list literal.
            Some(Token::At) => {
                self.pos += 1;
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.pos += 1;
                    let mut items = Vec::new();
                    loop {
                        if matches!(self.peek(), Some(Token::RParen)) {
                            self.pos += 1;
                            break;
                        }
                        items.push(self.additive()?);
                        match self.peek() {
                            Some(Token::Comma | Token::Semi) => self.pos += 1,
                            Some(Token::RParen) => {}
                            _ => {
                                return Err(QuarbError::parse(
                                    "a list literal separates its items with ';': @(a; b)",
                                ));
                            }
                        }
                    }
                    return Ok(Operand::List(items));
                }
                match self.peek() {
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) if text == "-" => {
                        self.pos += 1;
                        let projection = self.projection()?;
                        if matches!(
                            projection,
                            Some(Projection::CoreMeta(_) | Projection::AdapterMeta(_))
                        ) {
                            return Err(QuarbError::parse(
                                "an edge carries plain properties only (@-::prop)",
                            ));
                        }
                        Ok(Operand::Edges { projection })
                    }
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) if text == "." => {
                        self.pos += 1;
                        Ok(Operand::Recall(RegRef::Whole))
                    }
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) if text == "*" => {
                        self.pos += 1;
                        let projection = self.projection()?;
                        Ok(Operand::Capsae { projection })
                    }
                    _ => Err(QuarbError::parse(
                        "expected '-' (arrived edges), '.' (register), or '*' \
                         (the context) after '@' in an operand",
                    )),
                }
            }
            // `_` — the served node, projected or navigated like any
            // node (`_::prop`, `_/child::x`); bare, the node itself.
            Some(Token::Driver) => {
                self.pos += 1;
                return self.served_node();
            }
            Some(Token::Dollar) => {
                // Correlation context reference: $* or $*N, optionally
                // projected.
                self.pos += 1;
                // `$$N` — the join's N-th bound node: the driver's
                // doubled sigil with the position (`$*N` is the
                // heritage spelling). Any other `$$…` is the same
                // capsa-scope operand one scope out (the invoking
                // capsa of the enclosing subcontext); each extra `$`
                // steps out one more level.
                let mut joined: Option<usize> = None;
                if matches!(self.peek(), Some(Token::Dollar)) {
                    let digits = match self.toks.get(self.pos + 1) {
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) => {
                            Some(text.clone())
                        }
                        _ => None,
                    };
                    match digits {
                        Some(d) => {
                            let k = d.parse::<usize>().map_err(|_| {
                                QuarbError::parse(format!("bad join position '$${d}'"))
                            })?;
                            self.pos += 2;
                            if k == 0 {
                                // `$$0` — the served node, spelled `_`.
                                return self.served_node();
                            }
                            joined = Some(k);
                        }
                        None => {
                            return Err(QuarbError::parse(
                                "the capsa is shared: '$.name', '$_' and '$ord' read the same register \
                                 inside a body or a join condition; '_' is the served node, '$$N' a \
                                 bound node, '$$.name' a mark",
                            ));
                        }
                    }
                }
                let index = if joined.is_some() {
                    joined
                } else {
                    match self.peek() {
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text.starts_with('*') => {
                            let digits = text[1..].to_string();
                            self.pos += 1;
                            if digits.is_empty() {
                                None
                            } else {
                                return Err(QuarbError::parse(format!(
                                    "'$*{digits}' is retired: the join's N-th node is '$${digits}'"
                                )));
                            }
                        }
                        // `$.name` / `$.` — a register recall as an
                        // operand (the value pushed under that name).
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text.starts_with('.') => {
                            let rest = text[1..].to_string();
                            self.pos += 1;
                            let r = if rest.is_empty() {
                                RegRef::Top
                            } else if let Ok(n) = rest.parse::<usize>() {
                                RegRef::Index(n)
                            } else {
                                RegRef::Named(rest)
                            };
                            return Ok(Operand::Recall(r));
                        }
                        // `$_` — the topic (the current pipeline value).
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text == "_" => {
                            self.pos += 1;
                            return Ok(Operand::Topic);
                        }
                        // `$ordinal` / `$ord` — the capsa's position in
                        // the current context.
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text == "ord" => {
                            self.pos += 1;
                            return Ok(Operand::Ordinal);
                        }
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text == "ordinal" => {
                            return Err(QuarbError::parse("'$ordinal' is retired: write '$ord'"));
                        }
                        // `$1` … `$9` — a regex capture from the last
                        // successful `=~` match in a predicate command.
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text.chars().all(|c| c.is_ascii_digit()) => {
                            let n: usize = text.parse().map_err(|_| {
                                QuarbError::parse(format!("bad capture reference '${text}'"))
                            })?;
                            if n == 0 {
                                return Err(QuarbError::parse(
                                    "capture references are 1-based ('$1')",
                                ));
                            }
                            self.pos += 1;
                            return Ok(Operand::Capture(n));
                        }
                        // `$-` — the arrived-by edge; bare or with an
                        // edge-property projection (`$-::since`).
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text == "-" => {
                            self.pos += 1;
                            let projection = self.projection()?;
                            if matches!(
                                projection,
                                Some(Projection::CoreMeta(_) | Projection::AdapterMeta(_))
                            ) {
                                return Err(QuarbError::parse(
                                    "an edge carries plain properties only ($-::prop)",
                                ));
                            }
                            return Ok(Operand::Edge { projection });
                        }
                        // `$name` — a fragment parameter, inside a def
                        // body whose parameter list declares it.
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if self.def_params.iter().any(|p| p == text) => {
                            let name = text.clone();
                            self.pos += 1;
                            return Ok(Operand::Param(name));
                        }
                        _ => {
                            return Err(QuarbError::parse(
                                "expected '*N', '.name', '_', '-', or 'ord' after '$' in an operand",
                            ));
                        }
                    }
                };
                // Optional descending navigation from the bound context
                // node, then a projection — the same shape as a `Rel`
                // operand from the current node.
                let mut steps = Vec::new();
                while matches!(
                    self.peek(),
                    Some(
                        Token::Slash
                            | Token::SlashSlash
                            | Token::Backslash
                            | Token::BackslashBackslash
                            | Token::FollowingSiblings(_)
                            | Token::PrecedingSiblings(_)
                            | Token::NextSibling
                            | Token::PrevSibling
                            | Token::LParen
                    )
                ) {
                    steps.push(self.path_elem()?);
                }
                let projection = self.projection()?;
                Ok(Operand::Ctx {
                    index,
                    steps,
                    projection,
                })
            }
            // A fragment invocation as an operand: a predicate
            // fragment reads as a boolean, a navigation body as a
            // relative path (a bare one is an existence test, like
            // any path operand), a piped body under the inline
            // rules, a macro by its expansion.
            Some(Token::Amp) => self.splice_operand_fragment(),
            other => Err(QuarbError::parse(format!(
                "expected a value or path in a predicate, found {other:?}"
            ))),
        }
    }

    /// Expand a fragment invocation in operand position.
    fn splice_operand_fragment(&mut self) -> Result<Operand> {
        let (name, args, bang, quant) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::parse(format!("unknown fragment '&{name}'")));
        };
        if quant.is_some() {
            return Err(QuarbError::parse(format!(
                "a quantifier doesn't ride '&{name}' in operand position; \
                 put the quantified group inside the definition"
            )));
        }
        self.check_bang(&name, &def, bang)?;
        match def.body {
            DefBody::Predicates(preds) => {
                let map = self.bind(&name, &def.params, args)?;
                let subst = Subst {
                    outer: &map,
                    hole: &map,
                };
                let mut conj: Option<PredExpr> = None;
                for pred in preds {
                    let Predicate::Expr(mut e) = pred else {
                        return Err(QuarbError::parse(format!(
                            "the predicate fragment '&{name}' holds a \
                             positional predicate; a condition operand \
                             needs an expression"
                        )));
                    };
                    subst_pred_expr(&mut e, &subst);
                    conj = Some(match conj {
                        Some(prev) => PredExpr::And(Box::new(prev), Box::new(e)),
                        None => e,
                    });
                }
                let conj = conj.expect("a predicate body holds at least one predicate");
                Ok(Operand::Group(Box::new(conj)))
            }
            DefBody::Pipeline(_) => Err(QuarbError::parse(format!(
                "'&{name}' is a pipeline fragment; invoke it after a pipe"
            ))),
            DefBody::Query(mut q) => {
                let map = self.bind(&name, &def.params, args)?;
                subst_query(
                    &mut q,
                    &Subst {
                        outer: &map,
                        hole: &map,
                    },
                );
                self.operand_from_query(&name, q)
            }
            DefBody::Macro(_) => {
                let invited = invites_of(&args);
                let text = self.expand_macro_path_text(&name, &def, args)?;
                let wrap = |e: QuarbError| {
                    QuarbError::expansion(format!("in expansion of '&{name}' ('{text}')"))
                        .wrapping(e)
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                let mut p = self.child(&tokens, self.defs.before(&name))?;
                // Predicates reset pattern scope; the predicate
                // scope itself is inherited, so generated text
                // obeys the same restrictions hand-written
                // operands would (`<--` stays refused).
                p.predicate_depth = self.predicate_depth;
                let op = p.cond_expr().map_err(wrap)?;
                if p.pos != tokens.len() {
                    return Err(QuarbError::parse(format!(
                        "macro '&{name}' expanded to text with trailing \
                         content ('{text}')"
                    )));
                }
                self.record_captures(&name, &invited, crate::reflect::usage_of_operand(&op));
                Ok(op)
            }
        }
    }

    /// A navigation body as an operand: a relative path — any
    /// anchor, operand paths carry them — with the body's pipeline
    /// (if any) riding as an operand pipe tail under the inline
    /// rules.
    fn operand_from_query(&self, name: &str, q: Query) -> Result<Operand> {
        if !q.correlations.is_empty() {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' carries a correlation; a chain \
                 splices at driver position only"
            )));
        }
        let rel = if q.branches.len() == 1 {
            let b = q.branches.into_iter().next().expect("non-empty");
            Operand::Rel {
                steps: b.steps,
                projection: b.projection,
                anchor: b.anchor,
            }
        } else {
            if q.branches
                .iter()
                .any(|b| b.anchor != Anchor::Current || b.projection.is_some())
            {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' is a projected or anchored union; \
                     in operand position a union body must be plain \
                     navigation from the current node"
                )));
            }
            let alts: Vec<Vec<PathElem>> = q.branches.into_iter().map(|b| b.steps).collect();
            Operand::Rel {
                steps: vec![PathElem::Group(Group {
                    alts,
                    quant: Quant {
                        min: 1,
                        max: Some(1),
                    },
                    predicates: Vec::new(),
                    traits: Vec::new(),
                    reach: Reach::All,
                })],
                projection: None,
                anchor: Anchor::Current,
            }
        };
        // A def body parses at predicate depth zero, so re-validate
        // the one contextual restriction a splice could smuggle into
        // a predicate: reverse resolution scans the whole arbor per
        // candidate node.
        if self.predicate_depth > 0
            && let Operand::Rel { steps, .. } = &rel
            && walks_reverse_resolve(steps)
        {
            return Err(QuarbError::parse(format!(
                "fragment '&{name}' walks reverse resolution '<--', \
                 which is not allowed inside a predicate (it would scan \
                 the whole arbor per node); rewrite as a descending path \
                 or an incoming edge '<-'"
            )));
        }
        if q.pipeline.is_empty() {
            return Ok(rel);
        }
        for command in &q.pipeline {
            if matches!(
                command,
                Command::Push(_)
                    | Command::ExprPush { .. }
                    | Command::RecordPush { .. }
                    | Command::FieldsPush
                    | Command::Subcontext { .. }
                    | Command::AggPush { .. }
                    | Command::Nav(_)
            ) {
                return Err(QuarbError::parse(format!(
                    "fragment '&{name}' carries a pipeline that pushes or \
                     navigates; an operand pipe transforms a value \
                     (pushes belong to real capsae)"
                )));
            }
        }
        Ok(Operand::Piped {
            expr: Box::new(rel),
            commands: q.pipeline,
        })
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek()? {
            Token::Eq => CmpOp::Eq,
            Token::Ne => CmpOp::Ne,
            Token::Lt {
                spaced: true,
                before: true,
            } => CmpOp::Lt,
            Token::Le => CmpOp::Le,
            Token::Gt {
                spaced: true,
                before: true,
            } => CmpOp::Gt,
            Token::Ge => CmpOp::Ge,
            Token::MatchEq => CmpOp::Match,
            Token::NotMatchEq => CmpOp::NotMatch,
            Token::Contains => CmpOp::Contains,
            // The spelled ordering comparisons — a bare word in
            // operator position, like `and` / `or` / `not`.
            Token::Name {
                text,
                quoted: false,
                ..
            } => cmp_word(text)?,
            _ => return None,
        };
        self.pos += 1;
        Some(op)
    }

    /// A definition's head separator: the colon, spaced or glued
    /// (`def &f: body`, `def &f:body` — a glued colon lexes as the
    /// field colon, which reads the same here).
    fn expect_separator(&mut self, what: &str) -> Result<()> {
        if matches!(self.peek(), Some(Token::Field)) {
            self.pos += 1;
            return Ok(());
        }
        if matches!(self.peek(), Some(Token::Colon { .. })) {
            self.pos += 1;
            return Ok(());
        }
        Err(QuarbError::parse(format!("expected {what}")))
    }

    /// The conditional's `?` or `:` (spec: Lexical Disambiguation):
    /// written with a space on both sides, like every operator of
    /// the value grammar; glued on either side it is refused with
    /// the spaced spelling.
    fn binary_marker(&mut self, c: char) -> Result<()> {
        let spaced = match self.peek() {
            Some(Token::Question { spaced }) if c == '?' => *spaced,
            Some(Token::Colon { spaced }) if c == ':' => *spaced,
            _ => return Err(QuarbError::parse(format!("expected '{c}'"))),
        };
        if !spaced {
            return Err(QuarbError::parse(format!(
                "'{c}' is an operator and takes a space on both sides: write ' {c} '"
            )));
        }
        self.pos += 1;
        Ok(())
    }

    /// The conditional's else colon. A glued `:name` lexes as a
    /// field, `:0` as the colon glued to the literal; both are the
    /// else written without its spaces, and get the pointer.
    fn expect_else(&mut self, what: &str) -> Result<()> {
        match self.peek() {
            Some(Token::Colon { .. }) => self.binary_marker(':'),
            Some(Token::Field) => Err(QuarbError::parse(
                "':' is an operator and takes a space on both sides: write ' : '",
            )),
            _ => Err(QuarbError::parse(format!("expected {what}"))),
        }
    }

    /// Consume a bare word from `words` when an operand follows it
    /// — a name or an opening paren — so that a lone word closing a
    /// trait (`<and>`) stays a trait name.
    fn eat_word_if_followed(&mut self, words: &[&str]) -> bool {
        let followed = matches!(
            self.toks.get(self.pos + 1),
            Some(Token::Name { .. } | Token::LParen | Token::Bang)
        );
        followed && self.eat_word(words)
    }

    /// The served node after its token: an optional relative path
    /// and projection (`_::prop`, `_/child::x`), else the node.
    fn served_node(&mut self) -> Result<Operand> {
        let mut steps = Vec::new();
        while matches!(self.peek(), Some(Token::Slash | Token::SlashSlash)) {
            steps.push(self.path_elem()?);
        }
        let projection = self.projection()?;
        Ok(Operand::Outer(Box::new(Operand::Rel {
            steps,
            projection,
            anchor: Anchor::Current,
        })))
    }

    /// Is the next token a bare word from `words`?
    fn peek_word(&self, words: &[&str]) -> bool {
        matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if words.contains(&text.as_str()))
    }

    /// Consume the next token if it is a bare word from `words`.
    fn eat_word(&mut self, words: &[&str]) -> bool {
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && words.contains(&text.as_str())
        {
            self.pos += 1;
            return true;
        }
        false
    }

    fn expect(&mut self, tok: Token, what: &str) -> Result<()> {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            Ok(())
        } else {
            Err(QuarbError::parse(format!("expected {what}")))
        }
    }

    /// Try to parse a `<expr>` trait filter at the current position:
    /// a boolean expression over trait names — `||` (OR), `&&`
    /// (AND), tight `!` (NOT), parentheses — normalized to CNF at
    /// parse time, one [`TraitClause`] per conjunct (negated
    /// literals carry a leading `!`; `!*` = traitless). Returns
    /// `None` (without consuming) if what follows is not a
    /// well-formed trait\,---\,e.g. a bare `<name` that is really a
    /// previous-sibling hop.
    fn try_trait(&mut self) -> Result<Option<Vec<TraitClause>>> {
        if !matches!(self.peek(), Some(Token::Lt { spaced: false, .. })) {
            return Ok(None);
        }
        let start = self.pos;
        self.pos += 1; // consume '<'
        let Some(expr) = self.trait_or() else {
            self.pos = start;
            return Ok(None);
        };
        if !matches!(self.peek(), Some(Token::Gt { .. })) {
            self.pos = start;
            return Ok(None);
        }
        self.pos += 1;
        trait_cnf(expr).map(Some)
    }

    // The trait algebra takes the boolean words too (`<admin and
    // not banned>`, and their spellings in the family's languages):
    // a word in operator position is the operator; a lone word is
    // still a trait name.
    fn trait_or(&mut self) -> Option<TExpr> {
        let mut left = self.trait_and()?;
        while matches!(self.peek(), Some(Token::PipePipe)) || self.eat_word_if_followed(OR_WORDS) {
            if matches!(self.peek(), Some(Token::PipePipe)) {
                self.pos += 1;
            }
            let right = self.trait_and()?;
            left = TExpr::Or(Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn trait_and(&mut self) -> Option<TExpr> {
        let mut left = self.trait_not()?;
        while matches!(self.peek(), Some(Token::AmpAmp)) || self.eat_word_if_followed(AND_WORDS) {
            if matches!(self.peek(), Some(Token::AmpAmp)) {
                self.pos += 1;
            }
            let right = self.trait_not()?;
            left = TExpr::And(Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn trait_not(&mut self) -> Option<TExpr> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.pos += 1;
            return Some(TExpr::Not(Box::new(self.trait_not()?)));
        }
        if self.eat_word_if_followed(NOT_WORDS) {
            return Some(TExpr::Not(Box::new(self.trait_not()?)));
        }
        match self.peek() {
            Some(Token::Name { text, .. }) => {
                let mut name = text.clone();
                self.pos += 1;
                // A namespaced trait — `<tag:pandas>`,
                // `<category:Guides>`: inside the angle brackets no
                // record is in scope, so a glued colon joins the
                // name rather than reading a field.
                while matches!(self.peek(), Some(Token::Field))
                    && matches!(self.toks.get(self.pos + 1), Some(Token::Name { .. }))
                {
                    self.pos += 1;
                    if let Some(Token::Name { text, .. }) = self.bump() {
                        name.push(':');
                        name.push_str(text);
                    }
                }
                Some(TExpr::Has(name))
            }
            Some(Token::LParen) => {
                self.pos += 1;
                let inner = self.trait_or()?;
                if !matches!(self.peek(), Some(Token::RParen)) {
                    return None;
                }
                self.pos += 1;
                Some(inner)
            }
            _ => None,
        }
    }

    fn axis(&mut self) -> Result<Axis> {
        let axis = match self.bump() {
            Some(Token::Slash) => Axis::Child,
            Some(Token::SlashSlash) => Axis::Descendant(self.reach()),
            Some(Token::Backslash) => Axis::Parent,
            Some(Token::BackslashBackslash) => Axis::Ancestor(self.reach()),
            Some(Token::Gt { spaced: false, .. }) | Some(Token::NextSibling) => Axis::NextSibling,
            Some(Token::Lt { spaced: false, .. }) | Some(Token::PrevSibling) => Axis::PrevSibling,
            Some(t @ (Token::Gt { spaced: true, .. } | Token::Lt { spaced: true, .. })) => {
                let c = if matches!(t, Token::Gt { .. }) {
                    '>'
                } else {
                    '<'
                };
                return Err(QuarbError::parse(format!(
                    "'{c}' followed by a space is the comparison; the sibling hop \
                     is glued to its name: '{c}name' ('{c}*' for any)"
                )));
            }
            Some(Token::FollowingSiblings(mark)) => Axis::FollowingSiblings(mark_reach(*mark)),
            Some(Token::PrecedingSiblings(mark)) => Axis::PrecedingSiblings(mark_reach(*mark)),
            Some(Token::ArrowOut) => Axis::OutLink,
            Some(Token::ArrowIn) => Axis::InLink,
            Some(Token::DashDash) => Axis::BothLink,
            Some(Token::Name { text, .. }) => {
                return Err(QuarbError::parse(format!(
                    "expected a navigation operator before '{text}' \
                     (queries are root-anchored; start with '/', or open \
                     with an expression head: '= expr')"
                )));
            }
            // `:name` after a step: a node has properties, not
            // fields.
            Some(Token::Field) => {
                let name = match self.peek() {
                    Some(Token::Name { text, .. }) => text.clone(),
                    _ => "name".to_string(),
                };
                return Err(QuarbError::parse(format!(
                    "':{name}' reads a record's field; a node's property is '::{name}'"
                )));
            }
            _ => {
                return Err(QuarbError::parse(
                    "expected a navigation operator ('/', '//', '\\', …)",
                ));
            }
        };
        Ok(axis)
    }

    /// Consume an optional `?` (proximal) or `!` (distal) suffix.
    fn reach(&mut self) -> Reach {
        match self.peek() {
            Some(Token::Question { .. }) => {
                self.pos += 1;
                Reach::Proximal
            }
            Some(Token::Bang) => {
                self.pos += 1;
                Reach::Distal
            }
            _ => Reach::All,
        }
    }

    /// The matcher after an axis. The parent and ancestor axes take
    /// no name: the parent is one node and the ancestors are all of
    /// them, so a bare `\` / `\\` (or `\\?` / `\\!`) implies the
    /// wildcard — the one exception to "a hop names or patterns".
    /// A name after them is still the check it always was (`\x`).
    fn matcher_after(&mut self, axis: &Axis) -> Result<Matcher> {
        if matches!(axis, Axis::Parent | Axis::Ancestor(_))
            && (!matches!(
                self.peek(),
                Some(Token::Name { .. } | Token::Regex(_) | Token::Lt { .. })
            ) || self.peek_word(OR_WORDS))
        {
            return Ok(Matcher::Any);
        }
        self.matcher()
    }

    fn matcher(&mut self) -> Result<Matcher> {
        let in_pattern = self.pattern_depth > 0;
        // `/<block>` is sugar for `/*<block>`: a trait block directly
        // after an axis matches any node, the traits filtering it.
        // Leave the `<` for finish_step's trait parser to consume.
        if matches!(self.peek(), Some(Token::Lt { .. })) {
            return Ok(Matcher::Any);
        }
        match self.bump() {
            Some(Token::Name { text, quoted, .. }) => {
                // Inside a path pattern, a bare `.` is the dot
                // wildcard (any hop name); quote it for the literal.
                if !*quoted && text == "." && in_pattern {
                    return Ok(Matcher::Dot);
                }
                matcher_for(text, *quoted)
            }
            Some(Token::Regex(pat)) => Regex::new(pat)
                .map(Matcher::Regex)
                .map_err(|e| QuarbError::parse(format!("bad regex '~({pat})': {e}"))),
            _ => Err(QuarbError::parse(
                "a navigation operator must be followed by a name",
            )),
        }
    }
}

/// A trait boolean expression, as parsed; normalized to CNF
/// before it leaves the parser.
enum TExpr {
    Has(String),
    Not(Box<TExpr>),
    And(Box<TExpr>, Box<TExpr>),
    Or(Box<TExpr>, Box<TExpr>),
}

/// The clause count past which CNF conversion refuses: distributing
/// OR over AND is exponential, so an adversarial trait filter of a
/// few dozen OR'd conjunct pairs would otherwise hang the parse.
/// Real filters produce a handful of clauses.
const MAX_TRAIT_CLAUSES: usize = 512;

/// Normalize a trait expression to CNF: one clause per conjunct,
/// each a disjunction of literals (`name` / `!name`). This is what
/// lets the full algebra ride the executor's existing
/// AND-of-OR-clauses shape unchanged.
fn trait_cnf(e: TExpr) -> Result<Vec<TraitClause>> {
    // Negation-normal form first (De Morgan, double-negation).
    fn nnf(e: TExpr, neg: bool) -> TExpr {
        match e {
            TExpr::Not(inner) => nnf(*inner, !neg),
            TExpr::And(a, b) => {
                let (a, b) = (Box::new(nnf(*a, neg)), Box::new(nnf(*b, neg)));
                if neg {
                    TExpr::Or(a, b)
                } else {
                    TExpr::And(a, b)
                }
            }
            TExpr::Or(a, b) => {
                let (a, b) = (Box::new(nnf(*a, neg)), Box::new(nnf(*b, neg)));
                if neg {
                    TExpr::And(a, b)
                } else {
                    TExpr::Or(a, b)
                }
            }
            TExpr::Has(n) => {
                if neg {
                    TExpr::Has(format!("!{n}"))
                } else {
                    TExpr::Has(n)
                }
            }
        }
    }
    // Then distribute OR over AND, refusing past the clause budget.
    fn clauses(e: TExpr) -> Result<Vec<Vec<String>>> {
        let out = match e {
            TExpr::Has(n) => vec![vec![n]],
            TExpr::And(a, b) => {
                let mut out = clauses(*a)?;
                out.extend(clauses(*b)?);
                out
            }
            TExpr::Or(a, b) => {
                let (ca, cb) = (clauses(*a)?, clauses(*b)?);
                let mut out = Vec::with_capacity(ca.len().saturating_mul(cb.len()));
                for x in &ca {
                    for y in &cb {
                        let mut alt = x.clone();
                        alt.extend(y.iter().cloned());
                        out.push(alt);
                    }
                }
                out
            }
            TExpr::Not(_) => unreachable!("nnf removed compound negation"),
        };
        if out.len() > MAX_TRAIT_CLAUSES {
            return Err(QuarbError::parse(format!(
                "trait filter too complex: its normal form exceeds \
                 {MAX_TRAIT_CLAUSES} clauses"
            )));
        }
        Ok(out)
    }
    Ok(clauses(nnf(e, false))?
        .into_iter()
        .map(|alts| TraitClause { alts })
        .collect())
}

/// Interpret a name token as a predicate literal. A quoted name is
/// always a string; a bare name is a number, `true`/`false`/`null`, or
/// else a string.
/// An `@|` command: the name must be an aggregate (any family), and
/// its arguments pass the registry's check.
fn agg_command(call: FnCall) -> Result<Command> {
    // An environment command runs once over the context after `@|`.
    if crate::env::split_env(&call.name).is_some() {
        return Ok(Command::Agg(call));
    }
    let Some(spec) = crate::registry::spec(&call.name).filter(|s| s.agg.is_some()) else {
        return Err(QuarbError::unsupported(format!(
            "aggregate command '{}'",
            call.name
        )));
    };
    if call.name == "ungroup" && !call.args.is_empty() {
        return Err(QuarbError::parse("'ungroup' takes no arguments"));
    }
    match spec.check {
        crate::registry::Check::Command => validate_window_shift(&call)?,
        crate::registry::Check::Keyed => validate_keyed(&call)?,
        _ => {}
    }
    Ok(Command::Agg(call))
}

/// A `|` command: a scalar, or an aggregate that also works per capsa
/// — a keyed aggregate on a group's members (`@| group(::k) | top(2,
/// ::v)`), a reducing one on a list topic (`@| group(::k) | mean`).
/// A whole-context command (`ungroup`, `window`, `shift`) never takes
/// the plain pipe. The arguments pass the registry's check.
fn func_command(call: FnCall) -> Result<Command> {
    use crate::registry::{Agg, Check};
    // An environment command runs once per capsa after `|`.
    if crate::env::split_env(&call.name).is_some() {
        return Ok(Command::Func(call));
    }
    let spec = crate::registry::spec(&call.name);
    let stands = spec.is_some_and(|s| {
        s.scalar
            || matches!(
                s.agg,
                Some(Agg::Reducing | Agg::Keyed | Agg::Association { .. })
            )
    });
    if !stands {
        if let Some(pointer) = retired_command(&call.name) {
            return Err(QuarbError::parse(pointer));
        }
        let err = QuarbError::unsupported(format!("pipeline command '{}'", call.name));
        return Err(if spec.is_some_and(|s| s.agg == Some(Agg::Context)) {
            err.with_hint(format!(" ('{}' uses '@|')", call.name))
        } else {
            err
        });
    }
    match spec.map(|s| s.check) {
        Some(Check::Keyed) => validate_keyed(&call)?,
        _ => validate_record(&call)?,
    }
    Ok(Command::Func(call))
}

/// Check `record(...)`'s argument shape at parse time: fields come
/// either as a literal-string name followed by any argument, or as a
/// projection that names itself (`::href` → `href`); anything else
/// has no field name and is an error.
fn validate_record(call: &FnCall) -> Result<()> {
    // `decode`/`dec` take one argument: a bare scheme name that
    // must be reversible (`sha256` is one-way, so it is refused).
    if call.name == "decode" || call.name == "dec" {
        match call.args.as_slice() {
            [Arg::Lit(v)]
                if crate::encoding::is_decodable(&v.to_string())
                    || crate::encoding::is_structured_format(&v.to_string()) =>
            {
                return Ok(());
            }
            [Arg::Lit(v)] => {
                return Err(QuarbError::parse(format!(
                    "decode: '{}' is not a decodable format \
                     (base64, base64url, base32, crockford32, hex, \
                      json, yaml, toml, xml)",
                    v
                )));
            }
            _ => {
                return Err(QuarbError::parse(
                    "decode takes one scheme name, e.g. decode(base64)",
                ));
            }
        }
    }
    if !matches!(call.name.as_str(), "record" | "rec") {
        return Ok(());
    }
    validate_record_convention(call, "record")
}

/// The spelled comparison operator, for pointers.
fn cmp_text(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        CmpOp::Match => "==",
        CmpOp::NotMatch => "!==",
        CmpOp::Contains => "*=",
    }
}

/// The retired command names, each with its pointer: `rec` /
/// `record` (the record sigil `%(…)`) and `chars`
/// (the pre-0.25 spelling of `cc`).
pub(crate) fn retired_command(name: &str) -> Option<String> {
    match name {
        "rec" | "record" => Some(format!("'{name}(…)' is retired: the record is '%(…)'")),
        "chars" => Some("'chars' is retired: write 'cc'".into()),
        _ => None,
    }
}

/// The shared record-convention argument check, for `record(...)`
/// and `group(...)` keys.
/// The calls that follow the record convention — `key = value`
/// items, the flat `k, v` list, `;` separators.
fn is_record_convention(name: &str) -> bool {
    matches!(name, "rec" | "record" | "group")
}

fn validate_record_convention(call: &FnCall, what: &str) -> Result<()> {
    if call.args.is_empty() {
        // a record has fields; a grouping has keys
        let noun = if what == "group" { "key" } else { "field" };
        return Err(QuarbError::parse(format!(
            "{what} needs at least one {noun}, e.g. {what}(::name)"
        )));
    }
    // Field names are fully static — literals or auto-named
    // projections — so a collision is a parse error, never a
    // record with duplicate keys.
    let mut names: Vec<String> = Vec::new();
    let mut check = |name: &str| -> Result<()> {
        if names.iter().any(|n| n == name) {
            return Err(QuarbError::parse(format!(
                "{what} names the field '{name}' twice — name one \
                 explicitly: {what}(\"other-{name}\", ...)"
            )));
        }
        names.push(name.to_string());
        Ok(())
    };
    let mut i = 0;
    while i < call.args.len() {
        match &call.args[i] {
            // A literal string names the following argument.
            Arg::Lit(Value::Str(name)) => {
                if i + 1 >= call.args.len() {
                    return Err(QuarbError::parse(format!(
                        "{what} has a trailing field name with no value"
                    )));
                }
                check(name)?;
                i += 2;
            }
            Arg::Expr(e) => {
                let Some(name) = crate::ast::auto_field_name(e) else {
                    return Err(QuarbError::parse(format!(
                        "a {what} field needs a name: give a computed value a key, \
                         e.g. %(total = ::price * ::qty)"
                    )));
                };
                let name = name.to_string();
                check(&name)?;
                i += 1;
            }
            _ => {
                return Err(QuarbError::parse(format!(
                    "a {what} field needs a name: give a computed value a key, \
                     e.g. %(total = ::price * ::qty)"
                )));
            }
        }
    }
    Ok(())
}

/// Syntactic shape of a Unicode locale identifier (BCP 47):
/// hyphen-separated ASCII alphanumeric subtags of 1–8 chars, the
/// leading language subtag alphabetic of 2–8. Catches typos like
/// `sort(ru_RU!)` at parse time without deciding which locales
/// the collator actually supports.
fn valid_locale_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let Some(lang) = subtags.next() else {
        return false;
    };
    (2..=8).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_alphabetic())
        && subtags
            .all(|s| (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Check a keyed aggregate's argument shape at parse time: every
/// keyed command needs at least one expression key, and `top` /
/// `bottom` take a literal integer count first.
fn validate_keyed(call: &FnCall) -> Result<()> {
    // `group` follows the record convention for its keys (auto-named
    // projections, or literal-name-then-expression).
    if call.name == "group" {
        return validate_record_convention(call, "group");
    }
    // `sort` takes a Unicode locale identifier selecting the
    // collation (`sort(ru-RU)`), the word `atergo` selecting the
    // inverse-lexicographic order, or both, locale first
    // (`sort(ru-RU; atergo)`). The tag is validated here so a
    // typo fails the parse, not the sort. The check is syntactic
    // (BCP 47 shape) and deliberately feature-independent: a
    // query parses identically whether or not collation is
    // compiled in; existence and support are the collator's
    // concern at sort time.
    if call.name == "sort" {
        let locale = |v: &Value| -> Result<()> {
            let tag = v.to_string();
            if !valid_locale_tag(&tag) {
                return Err(QuarbError::parse(format!(
                    "sort: '{tag}' is not a Unicode locale identifier \
                     (try ru-RU, de-DE, zh-Hant, ...)"
                )));
            }
            Ok(())
        };
        let atergo = |v: &Value| matches!(v, Value::Str(s) if s == "atergo");
        match call.args.as_slice() {
            [] => return Ok(()),
            [Arg::Lit(v)] if atergo(v) => return Ok(()),
            [Arg::Lit(v)] => return locale(v),
            [Arg::Lit(loc), Arg::Lit(mode)] if atergo(mode) && !atergo(loc) => {
                return locale(loc);
            }
            _ => {
                return Err(QuarbError::parse(
                    "sort takes a locale identifier, the word atergo, or both \
                     with the locale first: sort, sort(ru-RU), sort(atergo), \
                     sort(ru-RU; atergo); keyed sorting is sort_by",
                ));
            }
        }
    }
    let keyed = matches!(
        call.name.as_str(),
        "sort_by" | "unique_by" | "min_by" | "max_by" | "top" | "bottom"
    );
    if !keyed {
        return Ok(());
    }
    let mut args = call.args.iter();
    if matches!(call.name.as_str(), "top" | "bottom")
        && !matches!(
            args.next(),
            Some(Arg::Lit(Value::Int(_)) | Arg::Range(_, _))
        )
    {
        return Err(QuarbError::parse(format!(
            "{} takes a count or a rank range first: {}(3; ::key), {}(11..20; ::key), {}(-10; ::key)",
            call.name, call.name, call.name, call.name
        )));
    }
    let mut keys = args.peekable();
    if keys.peek().is_none() || keys.any(|a| matches!(a, Arg::Lit(_))) {
        return Err(QuarbError::parse(format!(
            "{} needs value-expression keys, e.g. {}(::age)",
            call.name, call.name
        )));
    }
    Ok(())
}

/// Check `window` / `shift` argument shapes at parse time. `window`
/// takes an offset range (`window(-2..0)`, 0 = self, either end
/// optional) or a trailing count (`window(3)` ≡ `window(-2..0)`),
/// then an optional partition-key expression. `shift` takes an
/// integer distance (positive looks back, negative forward), then an
/// optional partition key.
fn validate_window_shift(call: &FnCall) -> Result<()> {
    let key_ok = |rest: &[Arg]| matches!(rest, [] | [Arg::Expr(_)]);
    match call.name.as_str() {
        "window" => match call.args.split_first() {
            Some((Arg::Range(a, b), rest)) if key_ok(rest) => {
                if let (Some(a), Some(b)) = (a, b)
                    && a > b
                {
                    return Err(QuarbError::parse(format!(
                        "window({a}..{b}) is empty: the range needs start <= end"
                    )));
                }
                Ok(())
            }
            Some((Arg::Lit(Value::Int(n)), rest)) if *n >= 1 && key_ok(rest) => Ok(()),
            _ => Err(QuarbError::parse(
                "window takes an offset range or a count, then an optional \
                 partition key: window(-2..0), window(3, ::group)",
            )),
        },
        "shift" => match call.args.split_first() {
            Some((Arg::Lit(Value::Int(_)), rest)) if key_ok(rest) => Ok(()),
            _ => Err(QuarbError::parse(
                "shift takes an integer distance, then an optional partition \
                 key: shift(1), shift(1, ::group)",
            )),
        },
        // `ngrams(n)`, and its two named sizes; `frequencies[(k)]`.
        "ngrams" => match call.args.as_slice() {
            [Arg::Lit(Value::Int(n))] if *n >= 1 => Ok(()),
            _ => Err(QuarbError::parse(
                "ngrams takes the size of the window: ngrams(2)",
            )),
        },
        "bigrams" | "trigrams" => {
            if call.args.is_empty() {
                Ok(())
            } else {
                Err(QuarbError::parse(format!(
                    "'{}' takes no arguments; a window of another size is ngrams(n)",
                    call.name
                )))
            }
        }
        "skipgrams" => match call.args.as_slice() {
            [Arg::Lit(Value::Int(n)), Arg::Lit(Value::Int(k))] if *n >= 1 && *k >= 0 => Ok(()),
            _ => Err(QuarbError::parse(
                "skipgrams takes the window size and the skips allowed: skipgrams(2; 1)",
            )),
        },
        "frequencies" => match call.args.as_slice() {
            [] => Ok(()),
            [Arg::Lit(Value::Int(k))] if *k != 0 => Ok(()),
            [Arg::Range(_, _)] => Ok(()),
            _ => Err(QuarbError::parse(
                "frequencies takes nothing, how many to keep, or a rank range: \
                 frequencies(10), frequencies(11..20), frequencies(-10)",
            )),
        },
        // `combinations(n)`, and its two named sizes.
        "combinations" => match call.args.as_slice() {
            [Arg::Lit(Value::Int(n))] if *n >= 1 => Ok(()),
            _ => Err(QuarbError::parse(
                "combinations takes the size of the subset: combinations(2)",
            )),
        },
        "pairs" | "triples" => {
            if call.args.is_empty() {
                Ok(())
            } else {
                Err(QuarbError::parse(format!(
                    "'{}' takes no arguments; a subset of another size is combinations(n)",
                    call.name
                )))
            }
        }
        // `csv[(a; b; …)]` / `table[(a; b; …)]` — the columns by name.
        "csv" | "table" => {
            if call.args.iter().all(|a| {
                matches!(a, Arg::Lit(Value::Str(_)))
                    || matches!(a, Arg::Named { name, .. } if name == "delimiter")
            }) {
                Ok(())
            } else {
                Err(QuarbError::parse(format!(
                    "{0} takes column names: {0}, {0}(year; title)",
                    call.name
                )))
            }
        }
        // `number[(level)]` — the level expression, or the value at
        // hand without one.
        "number" => match call.args.as_slice() {
            [] | [Arg::Expr(_)] => Ok(()),
            _ => Err(QuarbError::parse(
                "number takes an optional level expression: number, number(:::depth)",
            )),
        },
        // `timeline[(EXPR[; ANCHOR][; forward])]` — the capsa's time
        // expression, an anchor expression, and the word `forward`
        // in last position.
        "timeline" => {
            let args = match call.args.split_last() {
                Some((Arg::Lit(Value::Str(w)), rest)) if w == "forward" => rest,
                _ => call.args.as_slice(),
            };
            if args.len() <= 2 && args.iter().all(|a| !matches!(a, Arg::Range(..))) {
                Ok(())
            } else {
                Err(QuarbError::parse(
                    "timeline takes the time expression, an optional anchor, and \
                     the optional word forward: timeline($_), timeline($_; $.d), \
                     timeline($_; $.d; forward)",
                ))
            }
        }
        // `outline[(level; text[; "1."])]` — the two expressions, then
        // the numbering style; bare over a record stream.
        "outline" => match call.args.as_slice() {
            [] | [Arg::Expr(_), Arg::Expr(_)] => Ok(()),
            [Arg::Expr(_), Arg::Expr(_), Arg::Lit(Value::Str(style))] if style == "1." => Ok(()),
            _ => Err(QuarbError::parse(
                "outline takes a level and a text, then an optional numbering \
                 style: outline(:::depth; ::lemma), outline(:::depth; ::lemma; \"1.\")",
            )),
        },
        // `zscore([sample][; key])` — the word `sample` selects the
        // n − 1 spread, an expression partitions the standardizing.
        "zscore" => {
            let ok = match call.args.split_first() {
                None => true,
                Some((Arg::Lit(Value::Str(s)), rest)) if s == "sample" => key_ok(rest),
                Some((Arg::Expr(_), [])) => true,
                _ => false,
            };
            if ok {
                Ok(())
            } else {
                Err(QuarbError::parse(
                    "zscore takes the word sample and/or a partition key: \
                     zscore, zscore(sample), zscore($.w), zscore(sample; $.w)",
                ))
            }
        }
        _ => Ok(()),
    }
}

/// Check the statistics round's argument shapes wherever a call is
/// parsed (`|`, `@|`, the operand form): the dispersion measures
/// take an optional leading topic field and then only
/// the word `sample`; the association aggregates take exactly two
/// value expressions.
fn validate_stat_args(call: &FnCall) -> Result<()> {
    match call.name.as_str() {
        "std" | "var" | "stddev" | "variance" => {
            let rest = match call.args.split_first() {
                Some((Arg::Expr(_), rest)) => rest,
                _ => &call.args,
            };
            let ok = match rest {
                [] => true,
                [Arg::Lit(Value::Str(s))] => s == "sample",
                _ => false,
            };
            if ok {
                Ok(())
            } else {
                Err(QuarbError::parse(format!(
                    "{0} takes only the word sample: {0}, {0}(sample)",
                    call.name
                )))
            }
        }
        // `encoding(LABEL)` — the output boundary: one label the
        // Encoding Standard knows. The command stands last; the tool
        // that prints applies it (the engine has no byte value).
        "encoding" => match call.args.as_slice() {
            [Arg::Lit(Value::Str(label))] => {
                if crate::source::encoding_for_label(label).is_some() {
                    Ok(())
                } else {
                    Err(QuarbError::parse(format!(
                        "encoding: unknown label {label:?} (labels are the Encoding \
                         Standard's: utf-8, windows-1251, koi8-r, iso-8859-2, …)"
                    )))
                }
            }
            _ => Err(QuarbError::parse(
                "encoding takes one label: encoding(windows-1251)",
            )),
        },
        "corr" | "spearman" | "cosine" | "delta" | "dp" | "kappa" | "precision" | "recall"
        | "f1" => {
            if matches!(call.args.as_slice(), [Arg::Expr(_), Arg::Expr(_)]) {
                Ok(())
            } else {
                Err(QuarbError::parse(format!(
                    "{0} needs two value-expression keys: {0}(:a; :b), {0}(::x; ::y)",
                    call.name
                )))
            }
        }
        _ => Ok(()),
    }
}

/// The reach a lexed sibling-family mark carries.
fn mark_reach(mark: char) -> Reach {
    match mark {
        '?' => Reach::Proximal,
        '!' => Reach::Distal,
        _ => Reach::All,
    }
}

/// An identifier-shaped word: a letter or `_` first, then letters,
/// digits and `_`.
fn is_word(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn literal_value(text: &str, quoted: bool) -> Value {
    if quoted {
        return Value::Str(text.to_string());
    }
    match text {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if let Some(norm) = numeric_text(text) {
        if let Ok(n) = norm.parse::<i64>() {
            return Value::Int(n);
        }
        if let Ok(f) = norm.parse::<f64>()
            && f.is_finite()
        {
            return Value::Float(f);
        }
    }
    if let Ok(n) = text.parse::<i64>() {
        return Value::Int(n);
    }
    // The float reading is for digits only: Rust's f64 parser also
    // accepts the words `inf` / `infinity` / `NaN`, but a bare word
    // is a string literal here (`[::status = inf]` compares text).
    if text.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == '.')
        && let Ok(f) = text.parse::<f64>()
        && f.is_finite()
    {
        return Value::Float(f);
    }
    Value::Str(text.to_string())
}

/// A number's text in the parser's form: `_` group separators
/// stripped (they stand only between digits) and the decimal comma
/// read as the decimal point — `1_000,5` is `1000.5`. `None` when
/// the text is not shaped as a number this way.
fn numeric_text(text: &str) -> Option<String> {
    let body = text.strip_prefix('-').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    if text.starts_with('-') {
        out.push('-');
    }
    let mut seen_point = false;
    let chars: Vec<char> = body.chars().collect();
    if chars.first().is_none_or(|c| !c.is_ascii_digit()) {
        return None;
    }
    for (k, &c) in chars.iter().enumerate() {
        match c {
            d if d.is_ascii_digit() => out.push(d),
            '_' => {
                let between = k > 0
                    && chars[k - 1].is_ascii_digit()
                    && chars.get(k + 1).is_some_and(|n| n.is_ascii_digit());
                if !between {
                    return None;
                }
            }
            '.' | ',' => {
                if seen_point || !chars.get(k + 1).is_some_and(|n| n.is_ascii_digit()) {
                    return None;
                }
                seen_point = true;
                out.push('.');
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Whether `tok` begins a projection (ending the navigation part).
fn is_projection_start(tok: &Token) -> bool {
    matches!(
        tok,
        Token::ColonColon | Token::ColonColonColon | Token::SemiSemiSemi
    )
}

/// Build a [`Matcher`] from a name token. A quoted name is always a
/// literal; a bare name containing `*` is a glob.
fn matcher_for(text: &str, quoted: bool) -> Result<Matcher> {
    if quoted {
        return Ok(Matcher::Name(text.to_string()));
    }
    if text == "*" {
        return Ok(Matcher::Any);
    }
    if text.contains('*') {
        let glob =
            Glob::new(text).map_err(|e| QuarbError::parse(format!("bad glob '{text}': {e}")))?;
        return Ok(Matcher::Glob(glob.compile_matcher()));
    }
    Ok(Matcher::Name(text.to_string()))
}

/// Replace `$param` operands with the invocation's argument forms,
/// recursively through a cloned fragment body.
/// Parameter substitution maps: `outer` applies outside
/// interpolation holes, `hole` inside them. A template fragment
/// passes the same map for both (its holes evaluate the argument
/// form per capsa at run time); a macro binds the argument *form*
/// outside holes and its *text* (literals: their value) inside —
/// a hole is where syntax becomes text.
struct Subst<'a> {
    outer: &'a HashMap<String, Operand>,
    hole: &'a HashMap<String, Operand>,
}

fn subst_query(q: &mut Query, map: &Subst<'_>) {
    for corr in &mut q.correlations {
        subst_query(corr, map);
    }
    for b in &mut q.branches {
        if let Anchor::Id(op) = &mut b.anchor {
            subst_operand(op, map);
        }
        for elem in &mut b.steps {
            subst_elem(elem, map);
        }
    }
    for command in &mut q.pipeline {
        subst_command(command, map);
    }
}

fn subst_elem(elem: &mut PathElem, map: &Subst<'_>) {
    match elem {
        PathElem::Mark(_) => {}
        PathElem::Step(step) => subst_step(step, map),
        PathElem::Group(group) => {
            for alt in &mut group.alts {
                for elem in alt {
                    subst_elem(elem, map);
                }
            }
            for pred in &mut group.predicates {
                if let Predicate::Expr(e) = pred {
                    subst_pred_expr(e, map);
                }
            }
        }
        PathElem::Push { body, .. } => match body {
            PushBody::Query(q) => subst_query(q, map),
            PushBody::Expr(e) => subst_operand(e, map),
        },
    }
}

fn subst_step(step: &mut Step, map: &Subst<'_>) {
    for pred in &mut step.predicates {
        if let Predicate::Expr(e) = pred {
            subst_pred_expr(e, map);
        }
    }
}

fn subst_command(command: &mut Command, map: &Subst<'_>) {
    match command {
        Command::Func(call)
        | Command::Agg(call)
        | Command::RecordWith(call)
        | Command::RecordPush { call, .. } => {
            for arg in &mut call.args {
                if let Arg::Expr(e) | Arg::Opt { value: Some(e), .. } = arg {
                    subst_operand(e, map);
                }
            }
        }
        Command::Expr(e) | Command::ExprPush { expr: e, .. } | Command::AggPush { body: e, .. } => {
            subst_operand(e, map)
        }
        Command::AggRecord { call, .. } => {
            for arg in &mut call.args {
                if let Arg::Expr(e) | Arg::Opt { value: Some(e), .. } = arg {
                    subst_operand(e, map);
                }
            }
        }
        Command::Nav(b) => {
            if let Anchor::Id(op) = &mut b.anchor {
                subst_operand(op, map);
            }
            for elem in &mut b.steps {
                subst_elem(elem, map);
            }
        }
        Command::Subcontext { body, .. } => subst_query(body, map),
        Command::Filter(e) => subst_pred_expr(e, map),
        Command::Select(Predicate::Expr(e)) => subst_pred_expr(e, map),
        Command::Map(inner) | Command::Repeat { command: inner, .. } => subst_command(inner, map),
        Command::Select(_)
        | Command::Push(_)
        | Command::FieldsPush
        | Command::Recall(_)
        | Command::AggRecall(_)
        | Command::Spread { .. } => {}
    }
}

fn subst_pred_expr(e: &mut PredExpr, map: &Subst<'_>) {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            subst_pred_expr(a, map);
            subst_pred_expr(b, map);
        }
        PredExpr::Not(a) => subst_pred_expr(a, map),
        PredExpr::Compare(l, _, r) => {
            subst_operand(l, map);
            subst_operand(r, map);
        }
        PredExpr::Truthy(o) => subst_operand(o, map),
    }
}

fn subst_operand(o: &mut Operand, map: &Subst<'_>) {
    match o {
        // A pattern literal holds no operands: nothing to splice.
        Operand::Pattern(_) => {}
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            subst_operand(scrutinee, map);
            for (test, _, result) in arms {
                subst_operand(test, map);
                subst_operand(result, map);
            }
            subst_operand(other, map);
        }
        Operand::Param(name) => {
            if let Some(arg) = map.outer.get(name) {
                *o = arg.clone();
            }
        }
        Operand::Rel { steps, anchor, .. } => {
            if let Anchor::Id(op) = anchor {
                subst_operand(op, map);
            }
            for elem in steps {
                subst_elem(elem, map);
            }
        }
        Operand::Ctx { steps, .. } => {
            for elem in steps {
                subst_elem(elem, map);
            }
        }
        Operand::Arith { left, right, .. } => {
            subst_operand(left, map);
            subst_operand(right, map);
        }
        Operand::Neg(inner) => subst_operand(inner, map),
        Operand::Field { base, .. } | Operand::ValueMeta { base, .. } => subst_operand(base, map),
        Operand::PeerReg { at, .. } => subst_operand(at, map),
        Operand::List(items) => {
            for item in items {
                subst_operand(item, map);
            }
        }
        Operand::NamedCaptures => {}
        Operand::Group(e) => subst_pred_expr(e, map),
        Operand::Outer(inner) => subst_operand(inner, map),
        Operand::Interp(segs) => {
            let inside = Subst {
                outer: map.hole,
                hole: map.hole,
            };
            for seg in segs {
                if let InterpSeg::Expr(e) = seg {
                    subst_operand(e, &inside);
                }
            }
        }
        Operand::Piped { expr, commands } => {
            subst_operand(expr, map);
            for st in commands.iter_mut() {
                subst_command(st, map);
            }
        }
        Operand::Cond { cond, then, other } => {
            subst_pred_expr(cond, map);
            subst_operand(then, map);
            subst_operand(other, map);
        }
        Operand::Lit(_)
        | Operand::Recall(_)
        | Operand::Topic
        | Operand::Seed
        | Operand::Ordinal
        | Operand::Edge { .. }
        | Operand::Edges { .. }
        | Operand::Capsae { .. }
        | Operand::Capture(_)
        | Operand::Now => {}
    }
}

/// Enforce the driver-first placement of correlated references:
/// the first expression drives and cannot mention `$*k`; each
/// joined expression's ON clause (its final step's brackets) may
/// reference strictly earlier entries (and the driver as `$$…`);
/// the driver's pipeline reads any witness back.
/// A keyed aggregate — `top`, `bottom`, `sort_by`, `unique_by`,
/// `min_by`, `max_by` — ranks a context, so it rides `@|`; on the
/// plain pipe it works per group, after `@| group`. Without a group
/// command before it, it would rank each capsa's single value against
/// itself and pass everything through — refuse with the pointer.
/// The highest correlation context (`$*k`) a predicate expression
/// references, anywhere in it — nested bodies included.
pub(crate) fn max_ctx_pred_expr(e: &PredExpr) -> usize {
    let mut v = MaxCtx(0);
    v.pred(e);
    v.0
}

/// [`max_ctx_pred_expr`] over a path element.
fn max_ctx_elem(e: &PathElem) -> usize {
    let mut v = MaxCtx(0);
    v.elem(e);
    v.0
}

/// [`max_ctx_pred_expr`] over a pipeline command.
fn max_ctx_command(s: &Command) -> usize {
    let mut v = MaxCtx(0);
    v.command(s);
    v.0
}

struct MaxCtx(usize);

impl visit::Visitor for MaxCtx {
    fn operand(&mut self, o: &Operand) {
        if let Operand::Ctx { index, .. } = o {
            self.0 = self.0.max(index.unwrap_or(0));
        }
        visit::walk_operand(self, o);
    }
}

fn validate_keyed_commands(q: &Query) -> Result<()> {
    let mut grouped = false;
    for command in &q.pipeline {
        match command {
            // `@| group` and `@| window` partition the stream; a
            // keyed command on the plain pipe then works per part.
            Command::Agg(call) if matches!(call.name.as_str(), "group" | "window") => {
                grouped = true
            }
            Command::Func(call) if crate::stdlib::known_keyed(&call.name) && !grouped => {
                return Err(QuarbError::parse(format!(
                    "'{n}' ranks a context: write '@| {n}(…)' (on the plain pipe it works per group, after '@| group')",
                    n = call.name
                )));
            }
            _ => {}
        }
    }
    for c in &q.correlations {
        validate_keyed_commands(c)?;
    }
    Ok(())
}

fn validate_correlation_refs(q: &Query) -> Result<()> {
    let n = q.correlations.len();
    for b in &q.branches {
        for elem in &b.steps {
            if max_ctx_elem(elem) > 0 {
                return Err(QuarbError::parse(
                    "the first expression drives the join and cannot \
                     reference '$*k'; join conditions belong on the \
                     joined expression: 'A <=> B[::x = _::x]'",
                ));
            }
        }
    }
    for st in &q.pipeline {
        let m = max_ctx_command(st);
        if m > n {
            return Err(QuarbError::parse(format!(
                "the pipeline references '$*{m}', but only {n} \
                 expression(s) are joined"
            )));
        }
    }
    for (i, entry) in q.correlations.iter().enumerate() {
        let k = i + 1;
        for b in &entry.branches {
            let last = b.steps.len().saturating_sub(1);
            for (j, elem) in b.steps.iter().enumerate() {
                let m = max_ctx_elem(elem);
                if m == 0 && !elem_mentions_outer(elem) {
                    continue;
                }
                if j != last || !matches!(elem, PathElem::Step(_)) {
                    return Err(QuarbError::parse(
                        "join conditions belong in the joined \
                         expression's final step's brackets",
                    ));
                }
                if m >= k {
                    return Err(QuarbError::parse(format!(
                        "joined expression #{k} references '$*{m}'; it may \
                         only reference expressions joined before it \
                         ('$*1'..'$*{}') — the driver is '$$'",
                        k - 1
                    )));
                }
            }
            // A positional predicate after a correlated one would
            // select before the join takes effect — ambiguous.
            if let Some(PathElem::Step(s)) = b.steps.last() {
                let mut seen_corr = false;
                for p in &s.predicates {
                    match p {
                        Predicate::Expr(e)
                            if max_ctx_pred_expr(e) > 0 || pred_mentions_outer(e) =>
                        {
                            seen_corr = true;
                        }
                        Predicate::Index(_) | Predicate::Range(_, _) if seen_corr => {
                            return Err(QuarbError::parse(
                                "positional selection after a join condition \
                                 is ambiguous — select before the join \
                                 condition, or in the pipeline",
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        for st in &entry.pipeline {
            if max_ctx_command(st) > 0 {
                return Err(QuarbError::parse(
                    "a joined expression's own pipeline shapes its context \
                     before the join and cannot reference '$*k'; join \
                     conditions belong in its final step's brackets",
                ));
            }
        }
    }
    Ok(())
}

/// Whether a predicate expression reads the invoking capsa — an
/// outer reference (`$$…`), the register, the topic, the ordinal,
/// the captures, or a mark-anchored path. The capsa is shared: a
/// joined expression's predicate that reads it reads the DRIVER's,
/// so it is an ON condition, evaluated per driving capsa — not once
/// at materialization.
pub(crate) fn pred_mentions_outer(e: &PredExpr) -> bool {
    let mut v = Outer(false);
    v.pred(e);
    v.0
}

/// [`pred_mentions_outer`] over a path element.
pub(crate) fn elem_mentions_outer(e: &PathElem) -> bool {
    let mut v = Outer(false);
    v.elem(e);
    v.0
}

struct Outer(bool);

impl visit::Visitor for Outer {
    fn operand(&mut self, o: &Operand) {
        match o {
            Operand::Outer(_)
            | Operand::Recall(_)
            | Operand::Topic
            | Operand::Ordinal
            | Operand::Capture(_)
            | Operand::NamedCaptures => self.0 = true,
            Operand::Rel {
                anchor:
                    Anchor::Mark(_)
                    | Anchor::MarkIndex(_)
                    | Anchor::MarkTop
                    | Anchor::MarksAll
                    | Anchor::MarksNamed(_),
                ..
            } => self.0 = true,
            _ => visit::walk_operand(self, o),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arrow_opens_a_navigation_command() {
        // A command ending on a reference arrow is a hop — the reached
        // nodes are the capsae, as after `| ->x` — with or without a
        // projection after it; a trailing operator makes it the
        // structural-test expression, as parentheses always do.
        let first = |q: &str| {
            let p = parse(&lexer::lex(q).unwrap()).unwrap().pipeline;
            match &p[0] {
                Command::Nav(_) => "nav",
                Command::Expr(_) => "expr",
                _ => "other",
            }
        };
        assert_eq!(first("/a/* | ::artist_id-->"), "nav");
        assert_eq!(first("/a/* | ::artist_id-->::name"), "nav");
        assert_eq!(first("/a/* | ::artist_id=>(/b/*::id)"), "nav");
        assert_eq!(first("/a/* | ::artist_id=>(/b/*::id) | ::name"), "nav");
        assert_eq!(first("/a/* | -->"), "nav");
        assert_eq!(first("/a/* | <--(/b/*::artist_id)"), "nav");
        assert_eq!(first("/a/* | ::artist_id-->::n * 2"), "expr");
        assert_eq!(first("/a/* | (::artist_id--> @| count)"), "expr");
        assert_eq!(first("/a/* | ->artist_id"), "nav");
    }

    #[test]
    fn a_bare_sibling_hop_leads_an_operand() {
        // At the start of an operand `>name` / `<name` is the hop
        // (no comparison can stand there); the rounded spellings
        // parse to the same tree.
        let tree = |q: &str| format!("{:?}", parse(&lexer::lex(q).unwrap()).unwrap());
        assert_eq!(
            tree("//p[substr(::; 1..3) = substr(>p::; 1..3)]"),
            tree("//p[substr(::; 1..3) = substr(;-p::; 1..3)]")
        );
        assert_eq!(
            tree("/x | %(prev = <y::; next = >y::)"),
            tree("/x | %(prev = -;y::; next = ;-y::)")
        );
        assert_eq!(tree("//p[>p::n = 3]"), tree("//p[;-p::n = 3]"));
        // Spaced, `>` is the comparison; glued after an operand it is
        // refused with the pointer, as is a spaced hop.
        assert!(parse(&lexer::lex("//p[::n > 3]").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//p[substr(::; 1..3) > 3]").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//p[::n > 3 && ::m < 2]").unwrap()).is_ok());
        let e = parse(&lexer::lex("//p[::n>3]").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("spaced"), "{e}");
        let e = parse(&lexer::lex("//h2 > p::").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("glued"), "{e}");
        assert!(parse(&lexer::lex("//h2>p::").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//*<function>[::n > 1]").unwrap()).is_ok());
    }

    #[test]
    fn adversarial_nesting_is_an_error_not_an_abort() {
        // A long run of `(` must be refused by the depth bound —
        // unbounded recursion is a stack overflow (an abort).
        let deep = "(".repeat(5_000);
        let toks = lexer::lex(&deep).unwrap();
        assert!(parse(&toks).is_err());
        // `!` chains recurse through pred_not the same way.
        let bangs = format!("//a[{}::x]", "!".repeat(5_000));
        let toks = lexer::lex(&bangs).unwrap();
        assert!(parse(&toks).is_err());
        // Real nesting well under the bound still parses.
        let ok = format!("{}::x{}", "(".repeat(20), ")".repeat(20));
        let toks = lexer::lex(&format!("//a[{ok}]")).unwrap();
        assert!(parse(&toks).is_ok());
    }

    #[test]
    fn trait_cnf_blowup_is_an_error_not_a_hang() {
        // Distributing OR over AND is exponential: two dozen OR'd
        // conjunct pairs must refuse fast, not hang the parse.
        let pairs: Vec<String> = (0..24).map(|i| format!("(a{i} && b{i})")).collect();
        let toks = lexer::lex(&format!("//*<{}>", pairs.join(" || "))).unwrap();
        assert!(parse(&toks).is_err());
        // A modest algebra still normalizes.
        let toks = lexer::lex("//*<(a && b) || (c && d)>").unwrap();
        assert!(parse(&toks).is_ok());
    }

    #[test]
    fn record_field_names_must_be_unique() {
        for q in [
            "/r/* | %(::name; ::name)",
            "/r/* | %(a = ::x; a = ::y)",
            "/r/* | %(name = ::x; ::name)",
            "/r/* @| group(::k, ::k) | count",
        ] {
            let toks = lexer::lex(q).unwrap();
            let err = parse(&toks).expect_err(q).to_string();
            assert!(err.contains("twice"), "{q}: {err}");
        }
        // Distinct names stay fine, literal or auto-named.
        let toks = lexer::lex("/r/* | %(::name; other = ::name)").unwrap();
        assert!(parse(&toks).is_ok());
    }

    #[test]
    fn fragment_bodies_cover_chains_and_inline_pipes() {
        // A def may name a whole correlation; invoking it stands
        // alone, takes a pipe tail, and extends with further
        // entries.
        for q in [
            "def &j: /a/* <=> /b/*[::x = _::x]; &j @| count",
            "def &j: /a/* <=> /b/*[::x = _::x]; &j | [!$$1::x]",
            "def &j: /a/* <=> /b/*[::x = _::x]; &j <=> /c/*[::y = $$1::y]",
            // pipeline fragments splice inside inline pipes
            "def &g: | trim | upper; /r/* | .u((::name | &g))",
            "def &g2: | tp(\"%Y-%m-%d\") | ($_ + 12d); /r/* | .d((::when | &g2))",
        ] {
            let toks = lexer::lex(q).unwrap();
            assert!(parse(&toks).is_ok(), "should parse: {q}");
        }
        for (q, needle) in [
            // an entry cannot itself carry a correlation
            (
                "def &j: /a/* <=> /b/*[::x = _::x]; /d/* <=> &j",
                "chains are flat",
            ),
            // a chain fragment cannot join a union
            (
                "def &j: /a/* <=> /b/*[::x = _::x]; /d/* || &j",
                "stand alone",
            ),
            // a command-shaped def body points at the leading pipe
            ("def &p: .x(::a); /r/*", "starts with its pipe"),
            // spliced pushes stay out of inline pipes
            (
                "def &p: | .x(::a); /r/* | .u((::name | &p))",
                "may not push or navigate",
            ),
        ] {
            let toks = lexer::lex(q).unwrap();
            let err = parse(&toks).expect_err(q).to_string();
            assert!(err.contains(needle), "{q}: {err}");
        }
    }

    #[test]
    fn full_register_view_and_numeric_push_names() {
        // `%%.` parses as its own recall; `%.` stays itself. A push
        // name is an identifier or a quoted string glued to the dot
        // (the pivot macro writes data-valued column names,
        // `."1"(...)`); `%%.` keys anonymous slots `.N`, which no
        // push name can spell.
        for q in [
            "/r/* | .(::a) | %%.",
            "/r/* | .x(::a) | %. | %%.",
            "/r/* | .\"1\"(::a) | %.",
            "/r/* | %(a = 1; b = 2) | .%",
        ] {
            let toks = lexer::lex(q).unwrap();
            assert!(parse(&toks).is_ok(), "should parse: {q}");
        }
        let e = parse(&lexer::lex("/r/* | .1(::a) | %.").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("starts with a letter"), "{e}");
        let e = parse(&lexer::lex("/r/* | ._id(::a)").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("starts with a letter"), "{e}");
    }

    #[test]
    fn driver_first_placement_rules() {
        // Legal: ON on the joined expression ($$ = driver, $*k =
        // earlier entries), witnesses read back in the pipeline.
        for q in [
            "/a/* <=> /b/*[::x = _::x]",
            "/a/* <=>? /b/*[::x = _::x] | [!$$1::x]",
            "/a/* <=> /b/*[::x = _::x] <=> /c/*[::y = $$1::y] | %($$1::n; $$2::m)",
            "/a/* <=> /b/*[_/kid::x = ::x]",
        ] {
            let toks = lexer::lex(q).unwrap();
            assert!(parse(&toks).is_ok(), "should parse: {q}");
        }
        // The driver cannot reference the join from its own steps.
        for (q, needle) in [
            ("/a/*[::x = $$1::x] <=> /b/*", "first expression drives"),
            // A joined expression cannot reference itself or later
            // entries.
            ("/a/* <=> /b/*[::x = $$1::x]", "joined expression #1"),
            (
                "/a/* <=> /b/*[::x = $$2::x] <=> /c/*",
                "joined expression #1",
            ),
            // The pipeline cannot outrun the join count.
            ("/a/* <=> /b/* | %($$2::n)", "only 1 expression"),
            // Join conditions sit on the final step.
            ("/a/* <=> /b[_::x]/c/*", "final step"),
            // Positional selection after an ON clause is ambiguous.
            (
                "/a/* <=> /b/*[::x = _::x][1]",
                "positional selection after a join condition",
            ),
        ] {
            let toks = lexer::lex(q).unwrap();
            let err = parse(&toks).expect_err(q).to_string();
            assert!(err.contains(needle), "{q}: {err}");
        }
    }

    #[test]
    fn bare_conditionals_in_delimited_positions() {
        // A push body, a command operand, and a fragment parameter
        // are already parenthesized — a conditional inside them
        // needs no second pair.
        for q in [
            // push body: plain conditional, chained ladder, value match
            "/r/* | .born(::quarter ? 1890 : 0)",
            "/r/* | .born(::a ? 1 : ::b ? 2 : 3)",
            "/r/* | .port(::e ?= \"C\" ? 1 : \"Q\" ? 2 : 0)",
            // a path-existence condition
            "/r/* | .kind(/em ? \"has\" : \"none\")",
            // a pipe-tail branch inside the bare conditional
            "/r/* | .y(::quarter ? (::quarter | s/ Q.*$//) * 1 : 0)",
            // command operands: rec named field, group key
            "/r/* | %(age = ::Age ? ::Age * 1 : 0)",
            "/r/* @| group(\"src\", ::Age ? \"manifest\" : \"records\") | count",
            // fragment argument
            "def &f($x): /r/*[::a = $x]; &f(::b ? 1 : 2)",
            // the old double-parens spelling must keep parsing
            "/r/* | .born((::quarter ? 1890 : 0))",
            "/r/* | %(age = (( ::Age ? 1 : 0 )))",
        ] {
            let toks = lexer::lex(q).unwrap();
            assert!(parse(&toks).is_ok(), "should parse: {q}");
        }
    }

    #[test]
    fn overflowing_positional_index_is_an_error() {
        // Not a float operand whose truthiness keeps every node.
        let toks = lexer::lex("/a[9999999999999999999]").unwrap();
        assert!(parse(&toks).is_err());
    }

    #[test]
    fn options_close_a_simple_commands_parentheses() {
        let refuse = |q: &str| parse(&lexer::lex(q).unwrap()).unwrap_err().to_string();
        // No built-in declares an option yet: each is refused by
        // name, which also shows the segment parsed.
        assert!(refuse("/x | upper(-i)").contains("'upper' takes no options"), "{}", refuse("/x | upper(-i)"));
        let e = refuse("/x | ::n @| sort(::name; --locale=ru)");
        assert!(e.contains("'--locale' given"), "{e}");
        let e = refuse("/x | upper(--casefold -i)");
        assert!(e.contains("'--casefold' given"), "{e}");
        // Once in the segment every dash token is an option; the
        // void option opens a segment for a numeric flag.
        let e = refuse("/x | upper(-T=4 -9)");
        assert!(e.contains("'-T' given"), "{e}");
        let e = refuse("/x | upper(-= -9)");
        assert!(e.contains("'-9' given"), "{e}");
        // The options stand last.
        let e = refuse("/x | upper(a; -n=10; b)");
        assert!(e.contains("stand last"), "{e}");
        // A dash before a digit, `$` or a sigil is minus.
        parse(&lexer::lex("/x | ::n | round(-2)").unwrap()).unwrap();
        parse(&lexer::lex("/x | ::n | substr(-3..)").unwrap()).unwrap();
        // The canonical spelling: operands, then the segment; the
        // void option re-emitted where the segment would not open.
        let call = |args: Vec<Arg>| FnCall {
            name: "f".into(),
            args,
        };
        let opt = |n: &str, v: Option<Operand>| Arg::Opt {
            name: n.into(),
            value: v,
        };
        assert_eq!(
            crate::unparse::fn_call_text(&call(vec![
                Arg::Lit(Value::Str("a".into())),
                opt("-n", Some(Operand::Lit(Value::Int(10)))),
                opt("--locale", Some(Operand::Lit(Value::Str("ru".into())))),
                opt("-i", None),
            ])),
            "f(\"a\"; -n=10 --locale=ru -i)"
        );
        assert_eq!(
            crate::unparse::fn_call_text(&call(vec![opt("-9", None)])),
            "f(-= -9)"
        );
    }


    #[test]
    fn bare_inf_and_nan_are_not_numbers() {
        // Rust's f64 parser accepts the words; the query language
        // does not — a bare word in a body names a command, and
        // `inf` is none, so it is refused rather than read as a
        // number (or, as before, as text).
        let toks = lexer::lex("/x[::status = inf]").unwrap();
        let err = parse(&toks).unwrap_err().to_string();
        assert!(err.contains("'inf' is not a command"), "got {err}");
        let toks = lexer::lex("/x[::status = \"inf\"]").unwrap();
        let q = parse(&toks).unwrap();
        let dbg = format!("{q:?}");
        assert!(dbg.contains("Str(\"inf\")"), "got {dbg}");
    }

    fn last_step(q: &Query) -> &Step {
        match q.branches.last().unwrap().steps.last().unwrap() {
            PathElem::Step(s) => s,
            other => panic!("expected a step, got {other:?}"),
        }
    }

    #[test]
    fn map_pipe_after_step_is_not_leaf_anchor() {
        // `$|` glued to a navigation step is the map pipe, not a
        // leaf anchor followed by a plain pipe.
        let toks = lexer::lex("/data/tags $| upper").unwrap();
        let q = parse(&toks).unwrap();
        assert!(
            q.pipeline.iter().any(|s| matches!(s, Command::Map(_))),
            "expected a map command, got {:?}",
            q.pipeline
        );
        assert!(
            !last_step(&q).leaf,
            "the step preceding `$|` must !be leaf-anchored"
        );
    }

    #[test]
    fn bare_dollar_still_anchors_leaf() {
        // A `$` not glued to a pipe is still a leaf anchor.
        let toks = lexer::lex("/data/tags$").unwrap();
        let q = parse(&toks).unwrap();
        assert!(
            last_step(&q).leaf,
            "a bare trailing `$` anchors the step to leaves"
        );
    }

    #[test]
    fn macro_body_shell_is_gated_without_allow_shell() {
        // A macro body's shell command is evaluated at expansion (parse)
        // time; with no --allow-shell context it must be refused before
        // the command ever runs.
        let toks = lexer::lex("macro &m: ^ | `echo hi`; &m").unwrap();
        let err = parse(&toks).unwrap_err();
        assert!(
            err.to_string().contains("allow-shell"),
            "expected the shell gate to fire, got: {err}"
        );
    }

    #[test]
    fn trait_block_after_axis_is_wildcard_sugar() {
        // `/<block>` is sugar for `/*<block>`: a trait block right
        // after an axis matches any node. Both must parse identically.
        let sugar = parse(&lexer::lex("/<leaf>").unwrap()).unwrap();
        let full = parse(&lexer::lex("/*<leaf>").unwrap()).unwrap();
        assert_eq!(
            format!("{sugar:?}"),
            format!("{full:?}"),
            "'/<leaf>' must parse identically to '/*<leaf>'"
        );
    }

    #[test]
    fn reverse_resolution_refused_inside_predicate() {
        // `<--` walks the whole arbor per node, so it is refused inside a
        // predicate — but stays legal in top-level navigation, and the
        // bounded incoming edge `<-` is allowed in predicates.
        assert!(parse(&lexer::lex("//a[::r<--]").unwrap()).is_err());
        // The retired spelling: a property before the arrow is refused.
        assert!(parse(&lexer::lex("//a::r<--").unwrap()).is_err());
        assert!(parse(&lexer::lex("//a<--(::r)").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//a[<--(::r)]").unwrap()).is_err());
        assert!(parse(&lexer::lex("//a[<--(/x/*::r)]").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//a[<-b]").unwrap()).is_ok());
        // In an operand path the arrow continues a step chain:
        // `(<-book<--(::book) @| count)` counts the loans of a
        // genre's books.
        assert!(parse(&lexer::lex("//g | %(n = (<-b<--(::r) @| count))").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//g | %(n = (<--(::r)::x-->::y))").unwrap()).is_ok());
    }
}

/// Find a top-level bash hole operator: a `:` followed
/// by `?` or `-`, outside quotes, parens, brackets, and braces, and
/// not part of a `::` projection run (an ISO instant's time colon
/// is followed by a digit and never matches). Returns the byte
/// index of the `:` and the operator's spelling.
fn top_level_bash_op(src: &str) -> Option<(usize, &'static str)> {
    let chars: Vec<char> = src.chars().collect();
    let mut depth = 0i32;
    let mut i = 0usize;
    let mut byte = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' | '"' | '`' => {
                let q = c;
                byte += c.len_utf8();
                i += 1;
                while i < chars.len() {
                    let d = chars[i];
                    if d == '\\' && q != '\'' && i + 1 < chars.len() {
                        byte += d.len_utf8() + chars[i + 1].len_utf8();
                        i += 2;
                        continue;
                    }
                    byte += d.len_utf8();
                    i += 1;
                    if d == q {
                        break;
                    }
                }
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 => {
                if chars.get(i + 1) == Some(&':') {
                    while chars.get(i) == Some(&':') {
                        byte += 1;
                        i += 1;
                    }
                    continue;
                }
                match chars.get(i + 1) {
                    Some('?') => return Some((byte, ":?")),
                    Some('-') => return Some((byte, ":-")),
                    _ => {}
                }
            }
            _ => {}
        }
        byte += c.len_utf8();
        i += 1;
    }
    None
}

/// The spelled ordering comparisons — the rounded aliases of `<`,
/// `<=`, `>`, `>=` for keyboards where the angle brackets are
/// costly, and for typing in the user's own language. Latin takes
/// the comparatives (`minor quam`, `maior quam`) and their
/// negations for the bounds (`non maior` = at most); French the
/// `inférieur` / `supérieur` reading; Russian the quantity idioms
/// (`не более`, `не менее`); Greek the comparatives and the
/// `το πολύ` / `τουλάχιστον` idioms, with or without the tonos.
/// Each is a bare word spaced on both sides; the symbol is
/// canonical.
fn cmp_word(word: &str) -> Option<CmpOp> {
    Some(match word {
        // English (the Fortran heritage), French, Greek (tonos
        // optional), Latin, Russian, Spanish.
        ".lt." | ".inf." | ".μικρότερο." | ".μικροτερο." | ".minor." | ".менее." | ".menor." => {
            CmpOp::Lt
        }
        ".le." | ".nonsup." | ".τοπολύ." | ".τοπολυ." | ".nonmaior." | ".неболее."
        | ".nomayor." => CmpOp::Le,
        ".gt." | ".sup." | ".μεγαλύτερο." | ".μεγαλυτερο." | ".maior." | ".более." | ".mayor." => {
            CmpOp::Gt
        }
        ".ge."
        | ".noninf."
        | ".τουλάχιστον."
        | ".τουλαχιστον."
        | ".nonminor."
        | ".неменее."
        | ".nomenor." => CmpOp::Ge,
        _ => return None,
    })
}

/// The boolean words — the parsed aliases of `&&` / `||` / `!`,
/// which stay canonical — in each language of the rounded family,
/// dotted like the comparison words so that no bare word of any
/// language is a keyword: English `.and.` `.or.` `.not.`, French
/// `.et.` `.ou.` `.non.`, Greek `.και.` `.ή.` `.όχι.` (tonos
/// optional), Latin `.et.` `.vel.` `.non.`, Russian `.и.` `.или.`
/// `.не.`, Spanish `.y.` `.o.` `.no.`. The bare English words stay
/// as sugar (the programmer's habit). On layouts without `&` and
/// `|` the words are the only spelling of conjunction and
/// disjunction.
const AND_WORDS: &[&str] = &[".and.", ".et.", ".και.", ".и.", ".y."];
const OR_WORDS: &[&str] = &[".or.", ".ou.", ".ή.", ".η.", ".vel.", ".или.", ".o."];
const NOT_WORDS: &[&str] = &[".not.", ".non.", ".όχι.", ".οχι.", ".не.", ".no."];

/// The bare English words `and` / `or` / `not` were the last undotted
/// keywords; they are retired, and a bare one in operator position
/// is refused with the pointer.
const BARE_BOOL_WORDS: &[(&str, &str)] = &[("and", "&&"), ("or", "||"), ("not", "!")];

/// Whether a bare word is one of the boolean words — which, unquoted,
/// is never a property name.
pub(crate) fn is_bool_word(word: &str) -> bool {
    AND_WORDS.contains(&word) || OR_WORDS.contains(&word) || NOT_WORDS.contains(&word)
}

/// Turn the `//` step written after the `*` of a `*//` head into the
/// universal search: the step itself, or each alternative of a
/// name-alternation group (`*//(h1|h2|h3)`, the axis distributed).
/// A reach (`*//?`) and anything else are refused.
fn universalize(elem: &mut PathElem) -> Result<()> {
    fn step(s: &mut Step) -> Result<()> {
        match s.axis {
            Axis::Descendant(Reach::All) => {
                s.axis = Axis::Universal;
                Ok(())
            }
            _ => Err(QuarbError::parse(
                "'*//' searches the whole arbor and takes no reach ('*//?', '*//!')",
            )),
        }
    }
    match elem {
        PathElem::Step(s) => step(s),
        PathElem::Group(g) => {
            for alt in &mut g.alts {
                match alt.as_mut_slice() {
                    [PathElem::Step(s)] => step(s)?,
                    _ => {
                        return Err(QuarbError::parse(
                            "'*//(…)' takes name alternatives: each a single name or pattern",
                        ));
                    }
                }
            }
            Ok(())
        }
        _ => Err(QuarbError::parse(
            "'*//' is the universal search: follow it with a name, a pattern, or '*'",
        )),
    }
}
