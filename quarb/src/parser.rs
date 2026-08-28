//! Parser for the supported query subset.
//!
//! Grammar (sketch):
//!
//! ```text
//! parse   := query ('<=>' query)*         -- correlation chain
//! query   := branch ('||' branch)* stage*
//! branch  := '^'? step* projection?
//! step    := axis matcher trait* pred* '$'?
//! stage   := '|' (func | push | subcontext | recall) | '@|' func
//! pred    := '[' n ']' | '[' or_expr ']'
//! ```
//!
//! A recursive-descent parser; `parse_query` also nests inside a
//! subcontext body `.(…)`.

use crate::adapter::AstAdapter;
use crate::ast::{
    Anchor, Arg, ArithOp, Axis, Branch, CmpOp, FnCall, Group, InterpSeg, Matcher, Operand, PatSeg,
    PathElem,
    PredExpr, Predicate, Projection, PushBody, Quant, Query, Reach, RegRef, Stage, Step,
    TraitClause,
};
use crate::error::{QuarbError, Result};
use crate::lexer::{self, Token};
use crate::value::Value;
use globset::Glob;
use regex::Regex;
use std::collections::HashMap;

/// Parse a token stream into a [`Query`], expanding any inline
/// `def` statements.
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
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        defs,
        def_params: Vec::new(),
        data,
        pattern_depth: 0,
        predicate_depth: 0,
        nest_depth: 0,
        subquery_depth: 0,
        captures: std::cell::RefCell::new(Vec::new()),
        first_steps: std::cell::RefCell::new(Vec::new()),
    };
    p.parse()
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
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        defs,
        def_params: Vec::new(),
        data,
        pattern_depth: 0,
        predicate_depth: 0,
        nest_depth: 0,
        subquery_depth: 0,
        captures: std::cell::RefCell::new(Vec::new()),
        first_steps: std::cell::RefCell::new(Vec::new()),
    };
    p.parse()?;
    Ok(p.first_steps.into_inner())
}

/// Parse a token stream containing only `def` statements into a
/// fragment table (the `--defs` file format).
pub fn parse_defs(tokens: &[Token]) -> Result<Defs> {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        defs: Defs::default(),
        def_params: Vec::new(),
        data: None,
        pattern_depth: 0,
        predicate_depth: 0,
        nest_depth: 0,
        subquery_depth: 0,
        captures: std::cell::RefCell::new(Vec::new()),
        first_steps: std::cell::RefCell::new(Vec::new()),
    };
    while p.at_def() || p.at_macro() {
        if p.at_def() {
            p.parse_def()?;
        } else {
            p.parse_macro()?;
        }
    }
    if let Some(tok) = p.peek() {
        return Err(QuarbError::Parse(format!(
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
/// pipeline), a pipeline fragment (each stage's pipe is implied by
/// its variant), a predicate fragment (`def &vis: [cond];` — a
/// guard, spliced onto the step before it or read as a boolean
/// operand), or a procedural macro — a query evaluated at expansion
/// time against the invocation's expansion arbor, whose text
/// results become the expansion.
#[derive(Debug, Clone)]
enum DefBody {
    Query(Query),
    Pipeline(Vec<Stage>),
    Predicates(Vec<Predicate>),
    Macro(Query),
}

/// Which pipe introduces a stage — determined by its variant.
fn stage_pipe(stage: &Stage) -> &'static str {
    match stage {
        Stage::Agg(_) | Stage::Select(_) => "@|",
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
        reach,
    })]
}

/// Whether any step in these elements walks reverse resolution
/// (`<--`) — refused inside predicates, where a def body parsed at
/// depth zero could otherwise smuggle it in. Push bodies parse in
/// their own scopes and stay out of the walk.
fn walks_reverse_resolve(elems: &[PathElem]) -> bool {
    elems.iter().any(|e| match e {
        PathElem::Step(s) => matches!(s.axis, Axis::ReverseResolve { .. }),
        PathElem::Group(g) => g.alts.iter().any(|a| walks_reverse_resolve(a)),
        PathElem::Mark(_) | PathElem::Push { .. } => false,
    })
}

/// The static execution mode between pipeline stages (spec:
/// Execution Modes). Navigation mode: the thread is a node with no
/// live topic — hops may fan out. Scalar mode: a projection or
/// function has produced a live topic — a hop would drop it, so
/// path-shaped stages refuse until a push (`| .`) files the topic
/// and returns the thread to navigation mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PipeMode {
    Nav,
    Scalar,
}

/// A stage's mode-out given its mode-in. Pushes of every spelling
/// return to navigation mode (the push files the topic; spec:
/// "Pushes transition from scalar mode back to navigation mode").
/// Filters and positional selection preserve the mode; everything
/// that produces or transforms a topic enters scalar mode; a
/// navigation stage ends in the mode its optional projection
/// implies.
fn stage_mode_out(stage: &Stage, cur: PipeMode) -> PipeMode {
    match stage {
        Stage::Nav(b) => {
            if b.projection.is_some() {
                PipeMode::Scalar
            } else {
                PipeMode::Nav
            }
        }
        Stage::Push(_)
        | Stage::ExprPush { .. }
        | Stage::RecordPush { .. }
        | Stage::Subcontext { .. } => PipeMode::Nav,
        Stage::Filter(_) | Stage::Select(_) => cur,
        Stage::Func(_)
        | Stage::Expr(_)
        | Stage::Agg(_)
        | Stage::Recall(_)
        | Stage::RecordWith(_)
        | Stage::Spread { .. }
        | Stage::Map(_) => PipeMode::Scalar,
    }
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    /// Enter one level of recursive nesting; a parse error past the
    /// bound, instead of a stack overflow. Callers pair it with a
    /// decrement after the recursive call returns.
    fn descend(&mut self) -> Result<()> {
        self.nest_depth += 1;
        if self.nest_depth > MAX_NEST {
            return Err(QuarbError::Parse(format!(
                "query nested more than {MAX_NEST} levels deep"
            )));
        }
        Ok(())
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
            return Err(QuarbError::Parse("empty query".into()));
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
            // fields (ruling #48).
            if matches!(tok, Token::Field)
                && let Some(Token::Name { text, .. }) = self.toks.get(self.pos + 1)
            {
                return Err(QuarbError::Parse(format!(
                    "':{text}' reads a record's field; a node's property is '::{text}'"
                )));
            }
            return Err(QuarbError::Parse(format!(
                "unexpected trailing input at token {tok:?}"
            )));
        }
        validate_correlation_refs(&query)?;
        validate_keyed_stages(&query)?;
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
        while matches!(self.peek(), Some(Token::Correlate)) {
            self.pos += 1;
            // `<=>?` — the outer marker flags the expression it
            // precedes: that context may bind null (LEFT JOIN).
            let outer = if matches!(self.peek(), Some(Token::Question)) {
                self.pos += 1;
                true
            } else {
                false
            };
            let mut entry = self.parse_query()?;
            if !entry.correlations.is_empty() {
                return Err(QuarbError::Parse(
                    "a joined expression cannot itself carry a \
                     correlation — chains are flat; splice a \
                     chain-carrying fragment at driver position"
                        .into(),
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
        self.union_element(&mut branches, &mut pipeline, &mut correlations)?;
        while matches!(self.peek(), Some(Token::PipePipe)) {
            if !pipeline.is_empty() || !correlations.is_empty() {
                return Err(QuarbError::Parse(
                    "a fragment carrying a pipeline or a correlation must \
                     stand alone, not in a union"
                        .into(),
                ));
            }
            self.pos += 1;
            self.union_element(&mut branches, &mut pipeline, &mut correlations)?;
        }

        // Pipeline over the whole union. The entering mode is
        // static: a projected branch enters in scalar mode, plain
        // navigation in navigation mode; stages spliced from a
        // fragment have already advanced it.
        let mut mode = if branches.iter().any(|b| b.projection.is_some()) {
            PipeMode::Scalar
        } else {
            PipeMode::Nav
        };
        for s in &pipeline {
            mode = stage_mode_out(s, mode);
        }
        self.pipeline_items(&mut pipeline, mode)?;
        Ok(Query {
            correlations,
            outer: false,
            branches,
            pipeline,
        })
    }

    /// One element of a branch union: a plain branch, or a
    /// query-fragment invocation spliced in.
    fn union_element(
        &mut self,
        branches: &mut Vec<Branch>,
        pipeline: &mut Vec<Stage>,
        correlations: &mut Vec<Query>,
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
        if branches.is_empty()
            && self.subquery_depth == 0
            && matches!(self.peek(), Some(Token::Eq))
        {
            self.pos += 1;
            let expr = self.additive()?;
            branches.push(Branch {
                steps: Vec::new(),
                projection: None,
                anchor: Anchor::Root,
            });
            pipeline.push(Stage::Expr(expr));
            if matches!(self.peek(), Some(Token::PipePipe)) {
                return Err(QuarbError::Parse(
                    "an expression head stands alone; it cannot join a \
                     union — pipe from it instead: '= expr | …'"
                        .into(),
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
                return Err(QuarbError::Parse(format!(
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
                && (q.branches.len() == 1
                    || quant.is_some()
                    || self.splice_refinement_ahead())
            {
                let (elems, projection) =
                    self.finish_splice(&name, q.branches, quant, SplicePos::MidPath)?;
                branches.push(self.branch_tail(Anchor::Current, elems, projection)?);
                return Ok(());
            }
            if (!q.pipeline.is_empty() || !q.correlations.is_empty()) && !alone {
                return Err(QuarbError::Parse(
                    "a fragment carrying a pipeline or a correlation must \
                     stand alone, not in a union"
                        .into(),
                ));
            }
            branches.extend(q.branches);
            pipeline.extend(q.pipeline);
            correlations.extend(q.correlations);
            if matches!(self.peek(), Some(Token::LBracket)) {
                return Err(QuarbError::Parse(
                    "a fragment carrying a pipeline, a correlation, or an \
                     anchor does not take trailing predicates; refine it \
                     with a pipeline filter: '&name | [cond]'"
                        .into(),
                ));
            }
            if matches!(
                self.peek(),
                Some(Token::ColonColon | Token::ColonColonColon | Token::SemiSemiSemi)
            ) {
                return Err(QuarbError::Parse(
                    "a fragment carrying a pipeline, a correlation, or an \
                     anchor does not take a trailing projection; project \
                     it through the pipe: '&name | ::key'"
                        .into(),
                ));
            }
            if matches!(self.peek(), Some(Token::Lt)) {
                return Err(QuarbError::Parse(
                    "a fragment does not take a trailing trait selector; \
                     put the trait inside a definition: \
                     'def &errs: /entry<error> ;'"
                        .into(),
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
                | Token::Lt
                | Token::Gt
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

    /// Parse pipeline stages onto `pipeline` until something that is
    /// not a pipe. Shared by queries and pipeline-fragment bodies.
    /// `mode` is the static execution mode entering the first stage
    /// (spec: Execution Modes) — it decides whether a path-shaped
    /// stage is navigation or an error, and each stage's mode-out
    /// feeds the next.
    fn pipeline_items(&mut self, pipeline: &mut Vec<Stage>, mut mode: PipeMode) -> Result<()> {
        loop {
            match self.peek() {
                Some(Token::Pipe) => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(Token::Amp)) {
                        let before = pipeline.len();
                        self.invoke_pipeline_fragment("|", pipeline)?;
                        for s in &pipeline[before..] {
                            mode = stage_mode_out(s, mode);
                        }
                        continue;
                    }
                    let stage = self.pipe_item(mode)?;
                    mode = stage_mode_out(&stage, mode);
                    pipeline.push(stage);
                }
                // `$| stage` — the map pipe.
                Some(Token::Dollar) if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) => {
                    self.pos += 2;
                    let stage = Stage::Map(Box::new(self.map_stage()?));
                    mode = stage_mode_out(&stage, mode);
                    pipeline.push(stage);
                }
                Some(Token::At) => {
                    self.pos += 1;
                    self.expect(Token::Pipe, "'|' after '@' for aggregation")?;
                    if matches!(self.peek(), Some(Token::Amp)) {
                        let before = pipeline.len();
                        self.invoke_pipeline_fragment("@|", pipeline)?;
                        for s in &pipeline[before..] {
                            mode = stage_mode_out(s, mode);
                        }
                        continue;
                    }
                    // Hops are per-thread; `@|` aggregates the
                    // whole context.
                    if self.nav_stage_ahead() {
                        return Err(QuarbError::Parse(
                            "navigation is per-thread; write '| /path' \
                             ('@|' aggregates the whole context — hops don't take it)"
                                .into(),
                        ));
                    }
                    // `@| [n]` / `@| [a..b]` — positional selection
                    // from the whole context.
                    if matches!(self.peek(), Some(Token::LBracket)) {
                        match self.predicate()? {
                            pred @ (Predicate::Index(_) | Predicate::Range(_, _)) => {
                                // Select preserves the mode.
                                pipeline.push(Stage::Select(pred));
                                continue;
                            }
                            Predicate::Expr(_) => {
                                return Err(QuarbError::Parse(
                                    "a condition filters per capsa; write '| [cond]' \
                                     ('@| [n]' selects positionally)"
                                        .into(),
                                ));
                            }
                        }
                    }
                    let call = self.func_call()?;
                    if !crate::stdlib::known_agg(&call.name) {
                        return Err(QuarbError::Unsupported(format!(
                            "aggregate function '{}'",
                            call.name
                        )));
                    }
                    if call.name == "ungroup" && !call.args.is_empty() {
                        return Err(QuarbError::Parse("'ungroup' takes no arguments".into()));
                    }
                    validate_window_shift(&call)?;
                    validate_keyed(&call)?;
                    let stage = Stage::Agg(call);
                    mode = stage_mode_out(&stage, mode);
                    pipeline.push(stage);
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Parse one pipeline stage after a `|`: a navigation stage, a
    /// recall, a push, a subcontext, or a function. `mode` is the
    /// static execution mode entering this stage: path-shaped
    /// stages are navigation in navigation mode and an error in
    /// scalar mode (a live topic would be dropped — file it first).
    fn pipe_item(&mut self, mode: PipeMode) -> Result<Stage> {
        // A stage that starts like a path is a navigation stage —
        // the pipeline spelling of a path continuation.
        if self.nav_stage_ahead() {
            match mode {
                PipeMode::Scalar => {
                    // A parenthesized start keeps its expression
                    // reading (the documented escape); a bare axis
                    // start is a hop, and a hop would drop the
                    // live topic.
                    if !matches!(self.peek(), Some(Token::LParen)) {
                        return Err(QuarbError::Parse(
                            "cannot navigate in scalar mode — the topic is live \
                             and a hop would drop it; file it first with '| .' \
                             (or '| .name') and navigation resumes. \
                             (Parenthesize the path for a value expression.)"
                                .into(),
                        ));
                    }
                }
                PipeMode::Nav => {
                    // `(` is ambiguous: a quantified-walk group
                    // (`| (->e)+`) or a parenthesized expression
                    // (`| (/kids/*::name)`). Try the branch
                    // reading; fall back to the expression.
                    if matches!(self.peek(), Some(Token::LParen))
                        && !self.mark_anchor_ahead()
                    {
                        let save = self.pos;
                        if let Ok(b) = self.branch()
                            && !self.expr_continues()
                        {
                            return Ok(Stage::Nav(b));
                        }
                        self.pos = save;
                    } else {
                        // A dangling expression operator after the
                        // branch (`| /price:: * 2`) means the stage
                        // was arithmetic all along — keep the
                        // operand reading. A bare path is a hop.
                        let save = self.pos;
                        let b = self.branch()?;
                        if !self.expr_continues() {
                            return Ok(Stage::Nav(b));
                        }
                        self.pos = save;
                        return Ok(Stage::Expr(self.additive()?));
                    }
                }
            }
        }
        match self.peek() {
            // `| $.name`, `| $_`, `| $ord`, and any arithmetic over
            // them are value expressions; a plain recall is just the
            // single-operand case.
            Some(Token::Dollar) => Ok(Stage::Expr(self.additive()?)),
            // `| :name` — the topic record's field; `| %+` — the
            // named-captures record (ruling #48).
            Some(Token::Field | Token::PercentPlus) => Ok(Stage::Expr(self.additive()?)),
            // `| @(a; b)` — a list literal as the stage's expression.
            Some(Token::At) if matches!(self.toks.get(self.pos + 1), Some(Token::LParen)) => {
                Ok(Stage::Expr(self.additive()?))
            }
            Some(Token::At) => {
                // `| @-::prop` (and arithmetic over it) is a value
                // expression; `| @.` stays the whole-register recall.
                if matches!(
                    self.toks.get(self.pos + 1),
                    Some(Token::Name { text, quoted: false, .. }) if text == "-"
                ) {
                    return Ok(Stage::Expr(self.additive()?));
                }
                self.pos += 1;
                self.expect_dot("@")?;
                Ok(Stage::Recall(RegRef::Whole))
            }
            // A backtick literal is the sh(...) stage, sugared
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
                Ok(Stage::Func(FnCall {
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
                            return Err(QuarbError::Parse(
                                "an empty %%() adds nothing — the register view is %.".into(),
                            ));
                        }
                        validate_record(&call)?;
                        return Ok(Stage::RecordWith(call));
                    }
                    self.expect_dot("%%")?;
                    return Ok(Stage::Recall(RegRef::FullRecord));
                }
                if matches!(self.peek(), Some(Token::LParen)) {
                    let call = self.record_args()?;
                    validate_record(&call)?;
                    return Ok(Stage::Func(call));
                }
                self.expect_dot("%")?;
                Ok(Stage::Recall(RegRef::Record))
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
                let outer = if matches!(self.peek(), Some(Token::Question)) {
                    self.pos += 1;
                    true
                } else {
                    false
                };
                Ok(Stage::Spread { outer })
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
                let name = if text == "." {
                    None
                } else {
                    Some(text[1..].to_string())
                };
                // `.%(...)` / `.name%(...)` — the record push
                // (ruling #49): the constructor's fields, built and
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
                        return Err(QuarbError::Parse(
                            "a record push takes its fields: `.%(…)` or `.name%(…)`".into(),
                        ));
                    }
                    let call = self.record_args()?;
                    validate_record(&call)?;
                    return Ok(Stage::RecordPush {
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
                        return Ok(Stage::Subcontext {
                            name,
                            body: Box::new(body),
                        });
                    }
                    self.pos = save;
                    // The push's own parentheses delimit the body, so
                    // a conditional needs no second pair:
                    // `.born(::quarter ? … : …)`.
                    let expr = self.cond_expr()?;
                    self.expect(Token::RParen, "')' to close a subcontext")?;
                    Ok(Stage::ExprPush { name, expr })
                } else {
                    // In navigation mode a bare push marks; marks
                    // may not take numeric names (positions number
                    // themselves).
                    if mode == PipeMode::Nav
                        && name
                            .as_deref()
                            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
                    {
                        return Err(QuarbError::Parse(
                            "positions number themselves — a mark takes a \
                             word name ('| .name') or none at all ('| .'); \
                             recall a position with '(N)'"
                                .into(),
                        ));
                    }
                    Ok(Stage::Push(name))
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
                    .map_err(|e| QuarbError::Parse(format!("bad substitution pattern: {e}")))?;
                let call = FnCall {
                    name: "s".to_string(),
                    args: vec![
                        Arg::Lit(Value::Str(pattern.clone())),
                        Arg::Lit(Value::Str(replacement.clone())),
                        Arg::Lit(Value::Str(mods.clone())),
                    ],
                };
                self.pos += 1;
                Ok(Stage::Func(call))
            }
            // `| [cond]` — a per-capsa filter. Positional selection
            // is a whole-context operation and lives on `@|`.
            Some(Token::LBracket) => match self.predicate()? {
                Predicate::Expr(e) => Ok(Stage::Filter(e)),
                Predicate::Index(_) | Predicate::Range(_, _) => Err(QuarbError::Parse(
                    "positional selection is whole-context; write '@| [n]' / \
                     '@| [a..b]' (a plain '| [cond]' filters per capsa)"
                        .into(),
                )),
            },
            // A value-expression stage starts with a projection, a
            // parenthesized group, or an interpolated string:
            // `| ::price * ::qty`, `| "${::name} (${::age})"`.
            // (A bare path start is a navigation stage, handled
            // above; parenthesized paths remain expressions.)
            Some(
                Token::ColonColon
                | Token::ColonColonColon
                | Token::SemiSemiSemi
                | Token::LParen
                | Token::Interp(_),
            ) => Ok(Stage::Expr(self.additive()?)),
            // A quoted string is a constant-topic stage (`| 'text'`);
            // unquoted names remain function calls.
            Some(Token::Name { quoted: true, .. }) => Ok(Stage::Expr(self.additive()?)),
            _ => {
                let call = self.func_call()?;
                // `now()` refuses stage position: it takes no topic.
                if call.name == "now" {
                    return Err(QuarbError::Parse(
                        "now() is a call operand (the invocation instant); it takes no \
                         topic — use it in expression position: [::date > now() - 12h]"
                            .into(),
                    ));
                }
                // A keyed aggregate on the plain pipe works per capsa
                // on a group's members (`@| group(::k) | top(2, ::v)`).
                if crate::stdlib::known_keyed(&call.name) {
                    validate_keyed(&call)?;
                    return Ok(Stage::Func(call));
                }
                // Reducing aggregates are also per-capsa list
                // reductions (`@| group(::k) | mean` averages each
                // group's list topic). `ungroup`, `window`, and
                // `shift` read the whole context only.
                let reducible = crate::stdlib::known_agg(&call.name)
                    && !crate::stdlib::context_only(&call.name);
                if !crate::stdlib::known_scalar(&call.name) && !reducible {
                    let hint = if crate::stdlib::context_only(&call.name) {
                        format!(" ('{}' uses '@|')", call.name)
                    } else {
                        String::new()
                    };
                    return Err(QuarbError::Unsupported(format!(
                        "pipeline function '{}'{hint}",
                        call.name
                    )));
                }
                validate_record(&call)?;
                Ok(Stage::Func(call))
            }
        }
    }

    /// Whether the next tokens start a navigation stage: an axis
    /// token, the root anchor, a `(name)` mark anchor followed by a
    /// path continuation, or an axis-led group (a quantified walk).
    /// A `(` that opens anything else stays an expression.
    fn nav_stage_ahead(&self) -> bool {
        let axis = |t: &Token| {
            matches!(
                t,
                Token::Slash
                    | Token::SlashSlash
                    | Token::Backslash
                    | Token::BackslashBackslash
                    | Token::Gt
                    | Token::Lt
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
            Some(Token::LParen) => {
                self.mark_anchor_ahead()
                    || self.toks.get(self.pos + 1).is_some_and(axis)
            }
            _ => false,
        }
    }

    /// Whether the next token continues a value expression after a
    /// complete operand — an arithmetic operator or a conditional
    /// `?`. Decides branch-vs-expression for a path-shaped stage.
    fn expr_continues(&self) -> bool {
        match self.peek() {
            Some(Token::Name { text, quoted: false, .. }) => {
                matches!(text.as_str(), "+" | "-" | "*" | "div" | "idiv" | "mod")
            }
            Some(Token::Question) => true,
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
            _ => Err(QuarbError::Parse(format!("expected '.' after '{sigil}'"))),
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
                return Err(QuarbError::Parse(
                    "expected a fragment name after 'def &'".into(),
                ));
            }
        };
        if self.defs.get(&name).is_some() {
            return Err(QuarbError::Parse(format!(
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
                        return Err(QuarbError::Parse(
                            "expected a parameter name after '$'".into(),
                        ));
                    }
                };
                if param == "_"
                    || param == "ord"
                    || param == "ordinal"
                    || param.starts_with('.')
                    || param.starts_with('*')
                {
                    return Err(QuarbError::Parse(format!(
                        "parameter name '${param}' collides with a capsa-scope operand"
                    )));
                }
                params.push(param);
                if matches!(self.peek(), Some(Token::Comma)) {
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
            let mut stages = Vec::new();
            // A fragment body's splice-time mode is unknowable at
            // definition time; parse permissively (navigation mode)
            // — the splice site's own stages still typecheck.
            self.pipeline_items(&mut stages, PipeMode::Nav)?;
            if stages.is_empty() {
                return Err(QuarbError::Parse(format!(
                    "fragment '&{name}' has an empty body"
                )));
            }
            DefBody::Pipeline(stages)
        } else {
            // A stage-shaped body without its pipe is the classic
            // slip — point at the spelling instead of the grammar.
            if matches!(self.peek(),
                Some(Token::Name { text, quoted: false, .. }) if text.starts_with('.'))
                && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
            {
                return Err(QuarbError::Parse(format!(
                    "fragment '&{name}' body looks like a pipeline stage; \
                     a pipeline fragment starts with its pipe: \
                     'def &{name}: | .push(...) ;'"
                )));
            }
            // Placement of correlated references is checked at the
            // splice site, where the join's shape is known — not
            // here.
            DefBody::Query(self.parse_chain().map_err(|e| {
                QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(
                    "expected a macro name after 'macro &'".into(),
                ));
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
            return Err(QuarbError::Parse(format!(
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
                        return Err(QuarbError::Parse(
                            "expected '$name' or a trailing '@rest' parameter".into(),
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
                        return Err(QuarbError::Parse(
                            "expected a parameter name after its sigil".into(),
                        ));
                    }
                };
                if param == "_"
                    || param == "ord"
                    || param == "ordinal"
                    || param.starts_with('.')
                    || param.starts_with('*')
                {
                    return Err(QuarbError::Parse(format!(
                        "parameter name '{param}' collides with a capsa-scope operand"
                    )));
                }
                if data_aware && param == "data" {
                    return Err(QuarbError::Parse(
                        "a data-aware macro mounts the dataset as '/data'; \
                         pick another parameter name"
                            .into(),
                    ));
                }
                if is_rest {
                    rest = Some(param);
                    break;
                }
                params.push(param);
                if matches!(self.peek(), Some(Token::Comma)) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            self.expect(Token::RParen, "')' to close the parameter list")?;
        }
        self.expect_separator("':' between the macro name and its body")?;
        if matches!(self.peek(), Some(Token::Pipe | Token::At)) {
            return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
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
            return Err(QuarbError::Expansion(format!(
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
            return Err(QuarbError::Expansion(format!(
                "macro '&{name}' expanded to {} branch-shaped values; \
                 space-joined they would read as one path — join them \
                 into a union (@| join(' || ')) or emit a single group",
                values.len()
            )));
        }
        let text = values.join(" ");
        if text.trim().is_empty() {
            return Err(QuarbError::Expansion(format!(
                "macro '&{name}' expanded to nothing"
            )));
        }
        self.first_steps.borrow_mut().push(text.clone());
        Ok(text)
    }

    /// The expansion's raw output values (each `.to_string()`ed),
    /// before joining — see [`Self::expand_macro_text`].
    fn expand_macro_values(&self, name: &str, def: &Def, args: Vec<Operand>) -> Result<Vec<String>> {
        let n = def.params.len();
        let arity_ok = match def.rest {
            Some(_) => args.len() >= n,
            None => args.len() == n,
        };
        if !arity_ok {
            return Err(QuarbError::Parse(format!(
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
        // shell stage (`sh(...)` or a backtick) must clear the same
        // `--allow-shell` gate the top-level query does — otherwise a
        // macro body could run a command with no opt-in. The flag
        // rides on the dataset adapter (`self.data`); the bare
        // expansion arbor never allows shell on its own.
        let result = if def.data_aware {
            let Some(data) = self.data else {
                return Err(QuarbError::Parse(format!(
                    "macro '&{name}!' is data-aware: its expansion reads \
                     the dataset, so it needs an input (it cannot expand \
                     from query text alone)"
                )));
            };
            let combined = crate::reflect::ExpansionAdapter::new(arbor, data);
            crate::exec::gate_shell(&body, &combined)?;
            crate::exec::eval(&body, &combined)
        } else if self.data.is_some_and(|d| d.allow_shell()) {
            crate::exec::gate_shell(&body, &crate::adapter::AllowShell { inner: &arbor })?;
            crate::exec::eval(&body, &arbor)
        } else {
            crate::exec::gate_shell(&body, &arbor)?;
            crate::exec::eval(&body, &arbor)
        };
        let values = match result {
            crate::exec::QueryResult::Values(vs) => vs,
            crate::exec::QueryResult::Nodes(_) => {
                return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(
                    "expected a fragment name after '&'".into(),
                ));
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
                    // Same relaxation as function arguments: a
                    // conditional argument stands bare.
                    args.push(self.cond_expr()?);
                    if matches!(self.peek(), Some(Token::Comma)) {
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
            return Err(QuarbError::Parse(format!(
                "macro '&{name}' is data-aware (its expansion reads the \
                 dataset); invoke it as '&{name}!(...)'"
            )));
        }
        if !def.data_aware && bang {
            return Err(QuarbError::Parse(format!(
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
            return Err(QuarbError::Parse(format!(
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
            return Err(QuarbError::Parse(format!("unknown fragment '&{name}'")));
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
                return Err(QuarbError::Parse(format!(
                    "'&{name}' is a pipeline fragment; invoke it after a pipe"
                )));
            }
            DefBody::Predicates(_) => {
                return Err(QuarbError::Parse(format!(
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
                    QuarbError::Expansion(format!("in expansion of '&{name}' ('{text}'): {e}"))
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                if matches!(tokens.first(), Some(Token::Pipe | Token::At)) {
                    return Err(QuarbError::Expansion(format!(
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
        pipeline: &mut Vec<Stage>,
    ) -> Result<()> {
        let (name, args, bang, quant) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::Parse(format!("unknown fragment '&{name}'")));
        };
        if quant.is_some() {
            return Err(QuarbError::Parse(format!(
                "a quantifier doesn't ride the pipe; quantify '&{name}' \
                 at path position"
            )));
        }
        self.check_bang(&name, &def, bang)?;
        match def.body {
            DefBody::Pipeline(stages) => {
                let first = stage_pipe(stages.first().expect("non-empty checked at def time"));
                if first != pipe {
                    return Err(QuarbError::Parse(format!(
                        "fragment '&{name}' begins with '{first}' but was invoked with '{pipe}'"
                    )));
                }
                let map = self.bind(&name, &def.params, args)?;
                let subst = Subst {
                    outer: &map,
                    hole: &map,
                };
                for mut stage in stages {
                    subst_stage(&mut stage, &subst);
                    pipeline.push(stage);
                }
                Ok(())
            }
            DefBody::Query(_) => Err(QuarbError::Parse(format!(
                "'&{name}' is a query fragment; invoke it at path position"
            ))),
            // A predicate fragment on the plain pipe is the guard as
            // a per-capsa filter — `| &vis` ≡ `| [cond]` per predicate.
            DefBody::Predicates(preds) => {
                if pipe != "|" {
                    return Err(QuarbError::Parse(format!(
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
                            pipeline.push(Stage::Filter(e));
                        }
                        _ => {
                            return Err(QuarbError::Parse(format!(
                                "the predicate fragment '&{name}' holds a \
                                 positional predicate; select positionally \
                                 with '@| [n]'"
                            )));
                        }
                    }
                }
                Ok(())
            }
            // A macro expands to text; here it must be a stage
            // sequence whose first pipe matches the invocation.
            DefBody::Macro(_) => {
                let invited = invites_of(&args);
                let text = self.expand_macro_text(&name, &def, args)?;
                let wrap = |e: QuarbError| {
                    QuarbError::Expansion(format!("in expansion of '&{name}' ('{text}'): {e}"))
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                let first = match tokens.first() {
                    Some(Token::Pipe) => "|",
                    Some(Token::At) => "@|",
                    _ => {
                        return Err(QuarbError::Parse(format!(
                            "macro '&{name}' expanded to a query fragment \
                             ('{text}'); invoke it at path position — or \
                             have the body emit a leading '| ' to splice \
                             as pipeline stages"
                        )));
                    }
                };
                if first != pipe {
                    return Err(QuarbError::Parse(format!(
                        "macro '&{name}' expanded to a '{first}' pipeline \
                         ('{text}') but was invoked with '{pipe}'"
                    )));
                }
                let mut p = Parser {
                    toks: &tokens,
                    pos: 0,
                    defs: self.defs.before(&name),
                    def_params: Vec::new(),
                    data: self.data,
                    pattern_depth: 0,
                    predicate_depth: 0,
                    nest_depth: 0,
                    subquery_depth: 0,
                    captures: std::cell::RefCell::new(Vec::new()),
                    first_steps: std::cell::RefCell::new(Vec::new()),
                };
                let mut stages = Vec::new();
                p.pipeline_items(&mut stages, PipeMode::Nav).map_err(wrap)?;
                if p.pos != tokens.len() {
                    return Err(QuarbError::Parse(format!(
                        "macro '&{name}' expanded to text with trailing \
                         content ('{text}')"
                    )));
                }
                self.record_captures(&name, &invited, crate::reflect::usage_of_stages(&stages));
                pipeline.extend(stages);
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
            ) {
                break;
            }
            // A fragment invocation splices into the walk.
            if matches!(tok, Token::Amp) {
                projection = self.splice_path_fragment(&mut steps, SplicePos::MidPath)?;
                continue;
            }
            // `$|` at pipeline level is the map pipe — a stage, not a
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
                // `::name~>` is a resolution step (navigation
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
            return Err(QuarbError::Parse(
                "a query branch needs at least one step or a projection".into(),
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
            return Err(QuarbError::Parse(format!("unknown fragment '&{name}'")));
        };
        self.check_bang(&name, &def, bang)?;
        let q = match def.body {
            DefBody::Predicates(preds) => {
                if quant.is_some() {
                    return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
                    "fragment '&{name}' carries a pipeline; a pipe leaves \
                     navigation — invoke it at branch head or as a \
                     pipeline stage, not inside a walk"
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
            return Err(QuarbError::Parse(format!(
                "fragment '&{name}' carries a correlation; a chain \
                 splices at driver position only"
            )));
        }
        if !q.pipeline.is_empty() {
            return Err(QuarbError::Parse(format!(
                "fragment '&{name}' carries a pipeline; a pipe leaves \
                 navigation — invoke it at branch head or as a pipeline \
                 stage, not inside a walk"
            )));
        }
        if q.branches.iter().any(|b| b.anchor != Anchor::Current) {
            return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
                    "fragment '&{name}' ends in a projection; a group \
                     alternative walks on — invoke it where the branch ends"
                )));
            }
            if continues {
                return Err(QuarbError::Parse(format!(
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
        if matches!(self.peek(), Some(Token::Lt))
            && matches!(self.toks.get(self.pos + 1), Some(Token::Name { .. }))
            && matches!(self.toks.get(self.pos + 2), Some(Token::Gt))
            && !matches!(
                self.toks.get(self.pos + 3),
                Some(Token::Name { .. } | Token::Regex(_))
            )
        {
            return Err(QuarbError::Parse(
                "a fragment does not take a trailing trait selector; \
                 put the trait inside a definition: \
                 'def &errs: /entry<error> ;'"
                    .into(),
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
                    return Err(QuarbError::Parse(
                        "a group takes expression predicates only \
                         (positional selection has no order across \
                         repetition tiers)"
                            .into(),
                    ));
                }
                g.predicates.extend(preds);
                Ok(())
            }
            Some(PathElem::Mark(_) | PathElem::Push { .. }) => Err(QuarbError::Parse(format!(
                "the predicate fragment '&{name}' refines a hop or a \
                 group; a mark or a push takes no predicates"
            ))),
            None => Err(QuarbError::Parse(format!(
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
        let wrap =
            |e: QuarbError| QuarbError::Expansion(format!("in expansion of '&{name}' ('{text}'): {e}"));
        let tokens = lexer::lex(text).map_err(wrap)?;
        if matches!(tokens.first(), Some(Token::Pipe | Token::At)) {
            return Err(QuarbError::Expansion(format!(
                "macro '&{name}' expanded to a pipeline fragment \
                 ('{text}'); at path position it must expand to \
                 navigation steps"
            )));
        }
        let mut p = Parser {
            toks: &tokens,
            pos: 0,
            defs: self.defs.before(name),
            def_params: Vec::new(),
            data: self.data,
            pattern_depth: self.pattern_depth,
            predicate_depth: self.predicate_depth,
            nest_depth: 0,
            subquery_depth: 0,
            captures: std::cell::RefCell::new(Vec::new()),
            first_steps: std::cell::RefCell::new(Vec::new()),
        };
        p.parse().map_err(wrap)
    }

    /// Peek-only form of [`Self::mark_anchor`], for match guards.
    fn mark_anchor_ahead(&self) -> bool {
        self.peek_anchor().is_some()
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
        if !matches!(self.toks.get(self.pos), Some(Token::LParen)) {
            return None;
        }
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
                Some(Token::Name { text, quoted: false, .. })
                    if word(text)
                        && matches!(
                            self.toks.get(self.pos + 3),
                            Some(Token::RParen)
                        ) =>
                {
                    Some((Anchor::MarksNamed(text.clone()), 4))
                }
                _ => None,
            },
            Some(Token::Name { text, quoted: false, .. })
                if matches!(self.toks.get(self.pos + 2), Some(Token::RParen)) =>
            {
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
        let (anchor, len) = self.peek_anchor()?;
        self.pos += len;
        Some(anchor)
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
                return Err(QuarbError::Parse(
                    "expected a function name after '|'".into(),
                ));
            }
        };
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            // The record-convention calls (`rec`, `record`, `group`)
            // take `key = value` items and `;` between them as the
            // sigil forms do (ruling #50).
            let convention = is_record_convention(&name);
            if !matches!(self.peek(), Some(Token::RParen)) {
                loop {
                    if convention {
                        self.record_item(&mut args)?;
                    } else {
                        args.push(self.func_arg()?);
                    }
                    if matches!(self.peek(), Some(Token::Comma))
                        || (convention && matches!(self.peek(), Some(Token::Semi)))
                    {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen, "')' to close function arguments")?;
        }
        // A range argument is `window`'s span; nothing else takes one.
        if !matches!(name.as_str(), "window" | "substr")
            && args.iter().any(|a| matches!(a, Arg::Range(_, _)))
        {
            return Err(QuarbError::Parse(format!(
                "'{name}' takes no range argument ('window(a..b)' does)"
            )));
        }
        Ok(FnCall { name, args })
    }

    /// One function argument: a full value expression; a plain
    /// literal stays a literal. An offset range (`window(-2..0)`,
    /// either end optional) lexes as a single name token — digits,
    /// `-`, and `.` are all name characters — like the positional
    /// range predicate.
    /// One item of the record convention (ruling #50): `key = value`
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
            .ok_or_else(|| QuarbError::Parse(format!("{what} needs a key")))
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
            // `(N)`-addressable, and `(.)`/`(@)` see it.
            let name = if text == "." {
                None
            } else {
                let n = &text[1..];
                if n.chars().all(|c| c.is_ascii_digit()) {
                    return Err(QuarbError::Parse(
                        "positions number themselves — a mark takes a word \
                         name ('.name') or none at all ('.'); recall a \
                         position with '(N)'"
                            .into(),
                    ));
                }
                Some(n.to_string())
            };
            self.pos += 1;
            return Ok(PathElem::Mark(name));
        }
        if self.is_resolution_ahead() {
            return Ok(PathElem::Step(self.resolution_step()?));
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
                return Err(QuarbError::Parse(
                    "a quantifier attaches to a single-hop operator or a \
                     parenthesized group"
                        .into(),
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
                reach: self.reach(),
            }));
        }
        let matcher = self.matcher()?;
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
                    break Err(QuarbError::Parse(
                        "expected '|' or ')' in a path-pattern group".into(),
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
        let predicates = self.group_predicates()?;
        let reach = self.reach();
        Ok(Group {
            alts,
            quant,
            predicates,
            reach,
        })
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
                    return Err(QuarbError::Parse(
                        "a group takes expression predicates only \
                         (positional selection has no order across \
                         repetition tiers)"
                            .into(),
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
                    let matcher = self.matcher()?;
                    elems.push(PathElem::Step(self.finish_step(axis.clone(), matcher)?));
                }
                _ => {
                    return Err(QuarbError::Parse(
                        "the operator before '(' distributes over name \
                         alternatives; write the strict form '(/p|/div)'"
                            .into(),
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
            return Err(QuarbError::Parse(
                "a path-pattern alternative needs at least one hop".into(),
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
                    return Err(QuarbError::Parse(format!(
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
        // a stage the caller must see; consuming its `$` here would
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

    /// Whether the next tokens are `::name ~>` or `::name <~` (a
    /// forward or reverse resolution step).
    fn is_resolution_ahead(&self) -> bool {
        matches!(self.toks.get(self.pos), Some(Token::ColonColon))
            && matches!(self.toks.get(self.pos + 1), Some(Token::Name { .. }))
            && matches!(
                self.toks.get(self.pos + 2),
                Some(Token::Resolve | Token::ReverseResolve)
            )
    }

    /// Parse a resolution step `::property~>hint` (forward) or
    /// `::property<~hint` (reverse).
    fn resolution_step(&mut self) -> Result<Step> {
        self.pos += 1; // consume '::'
        let property = match self.bump() {
            Some(Token::Name { text, .. }) => text.clone(),
            _ => {
                return Err(QuarbError::Parse(
                    "expected a property name before '-->' or '<--'".into(),
                ));
            }
        };
        let reverse = matches!(self.bump(), Some(Token::ReverseResolve));
        if reverse && self.predicate_depth > 0 {
            // Reverse resolution scans the whole arbor per candidate
            // node; a predicate's nested paths must stay descending
            // (outgoing `->`/`-->` and incoming `<-` are fine).
            return Err(QuarbError::Parse(
                "reverse resolution '<--' is not allowed inside a predicate \
                 (it would scan the whole arbor per node); rewrite as a \
                 descending path or an incoming edge '<-'"
                    .into(),
            ));
        }
        // An optional relation hint (a bare name) follows.
        let hint = match self.peek() {
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
        };
        let axis = if reverse {
            Axis::ReverseResolve { property, hint }
        } else {
            Axis::Resolve { property, hint }
        };
        self.finish_step(axis, Matcher::Any)
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
                return Err(QuarbError::Parse(format!(
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

    fn pred_or(&mut self) -> Result<PredExpr> {
        let mut left = self.pred_and()?;
        while self.eat_word(OR_WORDS)
            || matches!(self.peek(), Some(Token::PipePipe)) && {
                self.pos += 1;
                true
            }
        {
            let right = self.pred_and()?;
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
        if let Some(op) = self.cmp_op() {
            // Ruling #44: a regex literal on the right of `=` / `!=`
            // is a pattern match — `= (/x/)` reads as `=~ (/x/)`, the
            // pattern doctrine of ruling #33.
            let op = match (op, self.peek()) {
                (CmpOp::Eq, Some(Token::Regex(_))) => CmpOp::Match,
                (CmpOp::Ne, Some(Token::Regex(_))) => CmpOp::NotMatch,
                (op, _) => op,
            };
            if matches!(op, CmpOp::Eq | CmpOp::Ne)
                && let Some(right) = self.pattern_operand()?
            {
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
    fn pattern_operand(&mut self) -> Result<Option<Operand>> {
        let start = self.pos;
        let star = |t: &Token| {
            matches!(t, Token::Name { text, quoted: false, .. } if text == "*")
        };
        let is_glued = |t: &Token| match t {
            Token::Name { glued, .. } => *glued,
            _ => false,
        };
        let mut segs: Vec<PatSeg> = Vec::new();
        loop {
            let Some(tok) = self.peek() else { break };
            let first = segs.is_empty();
            if !first && matches!(tok, Token::Interp(_)) {
                return Err(QuarbError::Parse(
                    "a pattern segment must be a literal string — \
                     an interpolated \"${...}\" hole cannot glob; \
                     use =~ for a dynamic pattern"
                        .into(),
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
                return Err(QuarbError::Parse(
                    "a pattern segment must be a quoted string or the \
                     glob star"
                        .into(),
                ));
            }
            return Ok(Some(Operand::Pattern(segs)));
        }
        if stars == 1 && lits == 0 {
            // The bare `= *` (and the spaced `= * \"x\"`): a lone
            // star is not a pattern.
            return Err(QuarbError::Parse(
                "a lone '*' is not a pattern — attach it to a string \
                 (*\"...\"*, \"...\"*), quote it ('*') to compare \
                 against the literal star, or test existence with the \
                 bare key"
                    .into(),
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
                _ => break,
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
        // `base:name` — a record's field (ruling #48), chained for
        // nested records; a leading `:name` reads the topic's field.
        let mut o = if matches!(self.peek(), Some(Token::Field)) {
            Operand::Topic
        } else {
            self.unary_primary()?
        };
        while matches!(self.peek(), Some(Token::Field)) {
            self.pos += 1;
            let name = match self.bump() {
                Some(Token::Name { text, .. }) => text.clone(),
                _ => return Err(QuarbError::Parse("expected a field name after ':'".into())),
            };
            if let Operand::Rel {
                projection: None, ..
            } = &o
            {
                return Err(QuarbError::Parse(format!(
                    "':{name}' reads a record's field; a node's property is '::{name}'"
                )));
            }
            o = Operand::Field {
                base: Box::new(o),
                name,
            };
        }
        Ok(o)
    }

    fn unary_primary(&mut self) -> Result<Operand> {
        if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "-") {
            self.pos += 1;
            return Ok(Operand::Neg(Box::new(self.unary()?)));
        }
        if matches!(self.peek(), Some(Token::LParen)) {
            // `(name)` followed by a path continuation is the mark
            // anchor — dispatch before the group readings claim the
            // paren.
            if self.mark_anchor_ahead() {
                return self.operand();
            }
            // A `(` here may open a path-pattern group (`[(->ref)+]`)
            // or a boolean/value group (`(::a or ::b)`). Try the path
            // reading first and back off on failure — a bare `(path)`
            // parses both ways with the same meaning, so preferring
            // the path changes nothing observable.
            let start = self.pos;
            match self.rel_from_group() {
                Ok(op) => return Ok(op),
                Err(_) => self.pos = start,
            }
            self.pos += 1;
            let inner = self.cond_expr()?;
            // An operand may carry a pipe tail inside its parens:
            // `(expr | f @| g)` — stage semantics, mirrored.
            let mut stages = Vec::new();
            loop {
                match self.peek() {
                    Some(Token::Pipe)
                        if matches!(self.toks.get(self.pos + 1), Some(Token::Amp)) =>
                    {
                        self.pos += 1;
                        self.invoke_inline_fragment("|", &mut stages)?;
                    }
                    Some(Token::Pipe) => {
                        self.pos += 1;
                        stages.push(self.inline_stage()?);
                    }
                    Some(Token::At)
                        if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe))
                            && matches!(self.toks.get(self.pos + 2), Some(Token::Amp)) =>
                    {
                        self.pos += 2;
                        self.invoke_inline_fragment("@|", &mut stages)?;
                    }
                    Some(Token::At) if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) => {
                        self.pos += 2;
                        stages.push(self.inline_agg_stage()?);
                    }
                    Some(Token::Dollar)
                        if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) =>
                    {
                        self.pos += 2;
                        stages.push(Stage::Map(Box::new(self.map_stage()?)));
                    }
                    _ => break,
                }
            }
            self.expect(Token::RParen, "')' to close a group")?;
            return Ok(if stages.is_empty() {
                inner
            } else {
                Operand::Piped {
                    expr: Box::new(inner),
                    stages,
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
    fn cond_expr(&mut self) -> Result<Operand> {
        let cond = self.pred_or()?;
        if matches!(self.peek(), Some(Token::QuestionEq)) {
            self.pos += 1;
            let PredExpr::Truthy(scrutinee) = cond else {
                return Err(QuarbError::Parse(
                    "the value match compares a VALUE: '(x ?= k ? r : else)'                      — a boolean condition belongs to the plain conditional"
                        .into(),
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
                if !matches!(self.peek(), Some(Token::Question)) {
                    // The first expression not followed by `?` is
                    // the else.
                    if regex {
                        return Err(QuarbError::Parse(
                            "a value match needs a final else after the                              regex arm: '(x ?= ~(pat) ? r : else)'"
                                .into(),
                        ));
                    }
                    break test;
                }
                self.pos += 1;
                let result = self.additive()?;
                self.expect(Token::Colon, "':' after a value-match arm")?;
                arms.push((test, regex, result));
            };
            if arms.is_empty() {
                return Err(QuarbError::Parse(
                    "a value match needs at least one arm:                      '(x ?= k ? r : else)'"
                        .into(),
                ));
            }
            return Ok(Operand::Match {
                scrutinee: Box::new(scrutinee),
                arms,
                other: Box::new(other),
            });
        }
        if matches!(self.peek(), Some(Token::Question)) {
            self.pos += 1;
            let then = self.cond_expr()?;
            self.expect(Token::Colon, "':' between the conditional's branches")?;
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
    /// stages, holding them to the inline rules — a pipe inside an
    /// expression transforms a value, so a spliced stage may not
    /// push or navigate.
    fn invoke_inline_fragment(
        &mut self,
        pipe: &'static str,
        stages: &mut Vec<Stage>,
    ) -> Result<()> {
        let before = stages.len();
        self.invoke_pipeline_fragment(pipe, stages)?;
        for stage in &stages[before..] {
            if matches!(
                stage,
                Stage::Push(_)
                    | Stage::ExprPush { .. }
                    | Stage::RecordPush { .. }
                    | Stage::Subcontext { .. }
                    | Stage::Nav(_)
            ) {
                return Err(QuarbError::Parse(
                    "a fragment spliced inside an expression pipe may \
                     not push or navigate (pushes belong to real \
                     capsae; use it as a pipeline stage instead)"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    /// One `| ...` stage of an inline pipe. The pipeline's own
    /// stage parser, minus what needs a real capsa: pushes and
    /// subcontexts refuse.
    fn inline_stage(&mut self) -> Result<Stage> {
        // Expression pipe tails live on the scalar plane; a
        // path-shaped stage errors in pipe_item before it can
        // become navigation.
        let stage = self.pipe_item(PipeMode::Scalar)?;
        match &stage {
            Stage::Push(_)
            | Stage::ExprPush { .. }
            | Stage::RecordPush { .. }
            | Stage::Subcontext { .. } => {
                Err(QuarbError::Parse(
                    "a pipe inside an expression transforms a value; \
                     pushes belong to real capsae (use a stage)"
                        .into(),
                ))
            }
            _ => Ok(stage),
        }
    }

    /// The stage a `$|` maps over the topic's elements: a
    /// positional predicate slices the list, an expression
    /// predicate filters its elements (`$_` = the element), and
    /// any per-value stage transforms them. Pushes and subcontexts
    /// refuse, as in inline pipes.
    fn map_stage(&mut self) -> Result<Stage> {
        // Hops are per-thread; `$|` transforms elements of a list
        // topic in place.
        if self.nav_stage_ahead() {
            return Err(QuarbError::Parse(
                "navigation doesn't ride the map pipe; '$|' transforms \
                 the elements of a list topic — write '| /path' as its \
                 own stage"
                    .into(),
            ));
        }
        if matches!(self.peek(), Some(Token::LBracket)) {
            let pred = self.predicate()?;
            return Ok(match pred {
                Predicate::Expr(e) => Stage::Filter(e),
                positional => Stage::Select(positional),
            });
        }
        self.inline_stage()
    }

    /// One `@| ...` stage of an inline pipe: an aggregate call, or
    /// positional selection.
    fn inline_agg_stage(&mut self) -> Result<Stage> {
        // The same discipline as the top-level `@|`: positional
        // selection only in brackets, known aggregates only, shapes
        // validated — an unchecked inline form would parse queries
        // whose conditions the executor silently ignores.
        if matches!(self.peek(), Some(Token::LBracket)) {
            return match self.predicate()? {
                pred @ (Predicate::Index(_) | Predicate::Range(_, _)) => Ok(Stage::Select(pred)),
                Predicate::Expr(_) => Err(QuarbError::Parse(
                    "a condition filters per capsa; write '| [cond]' \
                     ('@| [n]' selects positionally)"
                        .into(),
                )),
            };
        }
        let call = self.func_call()?;
        if !crate::stdlib::known_agg(&call.name) {
            return Err(QuarbError::Unsupported(format!(
                "aggregate function '{}'",
                call.name
            )));
        }
        if call.name == "ungroup" && !call.args.is_empty() {
            return Err(QuarbError::Parse("'ungroup' takes no arguments".into()));
        }
        validate_window_shift(&call)?;
        validate_keyed(&call)?;
        Ok(Stage::Agg(call))
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
    /// (`expr | stage ...`); mixing them wants the piped side
    /// parenthesized.
    fn parse_hole(&mut self, src: &str) -> Result<InterpSeg> {
        if let Some((idx, which)) = top_level_bash_op(src) {
            if idx == 0 || src[..idx].ends_with(|c: char| c.is_whitespace()) {
                return Err(QuarbError::Parse(format!(
                    "in '${{{src}}}': '{which}' glues to its expression \
                     (`${{expr{which}...}}`)"
                )));
            }
            let expr = self.hole_expr(&src[..idx], src)?;
            let tail = &src[idx + 2..];
            return Ok(if which == ":?" {
                let msg = tail.trim();
                InterpSeg::Strict(expr, (!msg.is_empty()).then(|| msg.to_string()))
            } else {
                InterpSeg::Default(expr, self.hole_expr(tail, src)?)
            });
        }
        let context = |e: QuarbError| QuarbError::Parse(format!("in '${{{src}}}': {e}"));
        let tokens = lexer::lex(src).map_err(context)?;
        let mut p = Parser {
            toks: &tokens,
            pos: 0,
            defs: self.defs.clone(),
            def_params: self.def_params.clone(),
            data: self.data,
            pattern_depth: 0,
            predicate_depth: 0,
            nest_depth: 0,
            subquery_depth: 0,
            captures: std::cell::RefCell::new(Vec::new()),
            first_steps: std::cell::RefCell::new(Vec::new()),
        };
        let expr = p.additive().map_err(context)?;
        // A pipe tail (ruling #34): the operand-pipe stages, exactly
        // as a parenthesized `(expr | f @| g)` carries them.
        let mut stages = Vec::new();
        loop {
            match p.peek() {
                Some(Token::Pipe) if matches!(p.toks.get(p.pos + 1), Some(Token::Amp)) => {
                    p.pos += 1;
                    p.invoke_inline_fragment("|", &mut stages).map_err(context)?;
                }
                Some(Token::Pipe) => {
                    p.pos += 1;
                    stages.push(p.inline_stage().map_err(context)?);
                }
                Some(Token::At)
                    if matches!(p.toks.get(p.pos + 1), Some(Token::Pipe))
                        && matches!(p.toks.get(p.pos + 2), Some(Token::Amp)) =>
                {
                    p.pos += 2;
                    p.invoke_inline_fragment("@|", &mut stages).map_err(context)?;
                }
                Some(Token::At) if matches!(p.toks.get(p.pos + 1), Some(Token::Pipe)) => {
                    p.pos += 2;
                    stages.push(p.inline_agg_stage().map_err(context)?);
                }
                _ => break,
            }
        }
        if p.pos != tokens.len() {
            return Err(QuarbError::Parse(format!(
                "in '${{{src}}}': an interpolation hole holds one value \
                 expression (with an optional pipe tail or glued ':?' / ':-')"
            )));
        }
        Ok(InterpSeg::Expr(if stages.is_empty() {
            expr
        } else {
            Operand::Piped {
                expr: Box::new(expr),
                stages,
            }
        }))
    }

    /// One side of a hole's bash operator: a value expression with
    /// no top-level pipe (the operator and a tail combine only with
    /// the piped side parenthesized).
    fn hole_expr(&mut self, part: &str, whole: &str) -> Result<Operand> {
        let context = |e: QuarbError| QuarbError::Parse(format!("in '${{{whole}}}': {e}"));
        let tokens = lexer::lex(part).map_err(context)?;
        let mut depth = 0i32;
        for t in &tokens {
            match t {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::Pipe if depth == 0 => {
                    return Err(QuarbError::Parse(format!(
                        "in '${{{whole}}}': a bash operator and a pipe tail \
                         are one or the other in a hole (parenthesize the \
                         piped expression)"
                    )));
                }
                _ => {}
            }
        }
        let mut p = Parser {
            toks: &tokens,
            pos: 0,
            defs: self.defs.clone(),
            def_params: self.def_params.clone(),
            data: self.data,
            pattern_depth: 0,
            predicate_depth: 0,
            nest_depth: 0,
            subquery_depth: 0,
            captures: std::cell::RefCell::new(Vec::new()),
            first_steps: std::cell::RefCell::new(Vec::new()),
        };
        let expr = p.additive().map_err(context)?;
        if p.pos != tokens.len() {
            return Err(QuarbError::Parse(format!(
                "in '${{{whole}}}': each side of ':?' / ':-' holds one \
                 value expression"
            )));
        }
        Ok(expr)
    }

    /// Parse a `$$…` outer-scope operand: consume the extra `$`s,
    /// parse the plain `$`-form, and wrap one `Outer` per extra
    /// sigil. Only capsa-scope operands step out; the reserved
    /// context-history accessor `$$*` is refused by name.
    fn outer_operand(&mut self) -> Result<Operand> {
        let mut depth = 0usize;
        while matches!(self.peek(), Some(Token::Dollar))
            && matches!(self.toks.get(self.pos + 1), Some(Token::Dollar))
        {
            self.pos += 1;
            depth += 1;
        }
        // `$$::prop`, `$$:::name`, `$$/child::x` — the node form:
        // the final `$` is the spelling's last dollar, and what
        // follows is a relative path/projection read from the
        // invoking capsa's node.
        let inner = if matches!(self.peek(), Some(Token::Dollar))
            && matches!(
                self.toks.get(self.pos + 1),
                Some(
                    Token::ColonColon
                        | Token::ColonColonColon
                        | Token::SemiSemiSemi
                        | Token::Slash
                        | Token::SlashSlash
                )
            ) {
            self.pos += 1;
            let mut steps = Vec::new();
            while matches!(self.peek(), Some(Token::Slash | Token::SlashSlash)) {
                steps.push(self.path_elem()?);
            }
            let projection = self.projection()?;
            Operand::Rel {
                steps,
                projection,
                anchor: Anchor::Current,
            }
        } else {
            self.operand()?
        };
        match inner {
            Operand::Recall(_) | Operand::Topic | Operand::Ordinal | Operand::Capture(_) => {}
            // `$$::prop`, `$$/child::x` — the invoking capsa's NODE,
            // navigated and projected like any relative path. This is
            // how a joined expression's ON clause reaches the driver
            // (`A <=> B[::uid = $$::id]`), and how a subcontext body
            // reaches the capsa it serves.
            Operand::Rel { .. } => {}
            Operand::Ctx { .. } => {
                return Err(QuarbError::Parse(
                    "the context-history accessor '$$*' is reserved (unbuilt);                      '$$' steps a capsa-scope operand out one level                      ($$.name, $$_, $$ord, $$::prop)"
                        .into(),
                ));
            }
            _ => {
                return Err(QuarbError::Parse(
                    "'$$' takes a capsa-scope operand ($$.name, $$_, $$ord, $$1, $$::prop)".into(),
                ));
            }
        }
        let mut out = inner;
        for _ in 0..depth {
            out = Operand::Outer(Box::new(out));
        }
        Ok(out)
    }

    fn operand(&mut self) -> Result<Operand> {
        match self.peek() {
            // A rounded-anchor operand path — `(name)`, `(N)`,
            // `(.)`, `(@)`, `(@name)`, `()` — navigates from the
            // anchored node(s). Same lookahead as the branch form;
            // parenthesized expressions keep `(` otherwise. The
            // plural forms gather values from every matching mark
            // (existential, like any multi-valued operand).
            Some(Token::LParen) if self.mark_anchor_ahead() => {
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
            Some(Token::Caret) => {
                self.pos += 1;
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
                if steps.is_empty() && projection.is_none() {
                    return Err(QuarbError::Parse(
                        "'^' in operand position starts a root-anchored path;                          follow it with steps or a projection"
                            .into(),
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
            // parent's name with `rec(dir, \*:::name)`. (The bare
            // `>` / `<` sibling hops are the comparison operators in
            // operand position; their rounded spellings serve.)
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
            ) => {
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
            // A resolution chain in operand position: follow the
            // reference(s), then project (`::album_id~>::title`).
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
            // A call operand: a function word glued to `(` — the
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
                        return Err(QuarbError::Parse(
                            "now() takes no arguments (it is the invocation instant)".into(),
                        ));
                    }
                    return Ok(Operand::Now);
                }
                // The record convention breaks the call-operand
                // duality: `rec("name", x)` reads name-first, but
                // `(x0 | rec(rest))` rides the first argument as the
                // topic — the field name becomes data and the record
                // silently re-keys. Refuse rather than re-key; the
                // pipe form spells the topic-riding meaning honestly.
                if matches!(call.name.as_str(), "rec" | "record") {
                    return Err(QuarbError::Parse(format!(
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
                    Some(Arg::Range(..)) => {
                        return Err(QuarbError::Parse(format!(
                            "'{}(...)' as an operand cannot ride a range as its topic",
                            call.name
                        )));
                    }
                    None => {
                        return Err(QuarbError::Parse(format!(
                            "a call operand needs a first argument to ride as the topic \
                             ('{0}(x)' is '(x | {0})'); only now() is nullary",
                            call.name
                        )));
                    }
                };
                // The duality must hold at parse time too: validate
                // the stage half exactly as `| f(rest)` would be —
                // otherwise `foo(::x)` parses here and its unparse
                // `(::x | foo)` fails its own reparse.
                let stage_call = FnCall {
                    name: call.name,
                    args: args.collect(),
                };
                if crate::stdlib::known_keyed(&stage_call.name) {
                    validate_keyed(&stage_call)?;
                } else {
                    let reducible = crate::stdlib::known_agg(&stage_call.name)
                        && !crate::stdlib::context_only(&stage_call.name);
                    if !crate::stdlib::known_scalar(&stage_call.name) && !reducible {
                        let hint = if crate::stdlib::context_only(&stage_call.name) {
                            format!(" ('{}' uses '@|')", stage_call.name)
                        } else {
                            String::new()
                        };
                        return Err(QuarbError::Unsupported(format!(
                            "pipeline function '{}'{hint}",
                            stage_call.name
                        )));
                    }
                    validate_record(&stage_call)?;
                }
                Ok(Operand::Piped {
                    expr: Box::new(first),
                    stages: vec![Stage::Func(stage_call)],
                })
            }
            Some(Token::Name { text, quoted, .. }) => {
                let value = literal_value(text, *quoted);
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
            // `%+` — the named-captures record (ruling #48).
            Some(Token::PercentPlus) => {
                self.pos += 1;
                Ok(Operand::NamedCaptures)
            }
            // `@-` — all arrived-by edges (bare: labels; projected:
            // each edge's property); `@.` — the whole register;
            // `@(…)` — the list literal (ruling #52).
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
                                return Err(QuarbError::Parse(
                                    "a list literal separates its items with ';': @(a; b)".into(),
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
                            return Err(QuarbError::Parse(
                                "an edge carries plain properties only (@-::prop)".into(),
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
                    _ => Err(QuarbError::Parse(
                        "expected '-' (arrived edges), '.' (register), or '*' \
                         (the context) after '@' in an operand"
                            .into(),
                    )),
                }
            }
            Some(Token::Dollar) => {
                // Correlation context reference: $* or $*N, optionally
                // projected.
                self.pos += 1;
                // `$$…` — the same capsa-scope operand one scope out
                // (the invoking capsa of the enclosing subcontext);
                // each extra `$` steps out one more level.
                if matches!(self.peek(), Some(Token::Dollar)) {
                    self.pos -= 1;
                    return self.outer_operand();
                }
                let index = match self.peek() {
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
                            Some(digits.parse::<usize>().map_err(|_| {
                                QuarbError::Parse(format!("bad context index '$*{digits}'"))
                            })?)
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
                    }) if text == "ordinal" || text == "ord" => {
                        self.pos += 1;
                        return Ok(Operand::Ordinal);
                    }
                    // `$1` … `$9` — a regex capture from the last
                    // successful `=~` match in a filter stage.
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) if text.chars().all(|c| c.is_ascii_digit()) => {
                        let n: usize = text.parse().map_err(|_| {
                            QuarbError::Parse(format!("bad capture reference '${text}'"))
                        })?;
                        if n == 0 {
                            return Err(QuarbError::Parse(
                                "capture references are 1-based ('$1')".into(),
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
                            return Err(QuarbError::Parse(
                                "an edge carries plain properties only ($-::prop)".into(),
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
                        return Err(QuarbError::Parse(
                            "expected '*N', '.name', '_', '-', or 'ord' after '$' in an operand"
                                .into(),
                        ));
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
            other => Err(QuarbError::Parse(format!(
                "expected a value or path in a predicate, found {other:?}"
            ))),
        }
    }

    /// Expand a fragment invocation in operand position.
    fn splice_operand_fragment(&mut self) -> Result<Operand> {
        let (name, args, bang, quant) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::Parse(format!("unknown fragment '&{name}'")));
        };
        if quant.is_some() {
            return Err(QuarbError::Parse(format!(
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
                        return Err(QuarbError::Parse(format!(
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
            DefBody::Pipeline(_) => Err(QuarbError::Parse(format!(
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
                    QuarbError::Expansion(format!("in expansion of '&{name}' ('{text}'): {e}"))
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                let mut p = Parser {
                    toks: &tokens,
                    pos: 0,
                    defs: self.defs.before(&name),
                    def_params: Vec::new(),
                    data: self.data,
                    // Predicates reset pattern scope; the predicate
                    // scope itself is inherited, so generated text
                    // obeys the same restrictions hand-written
                    // operands would (`<--` stays refused).
                    pattern_depth: 0,
                    predicate_depth: self.predicate_depth,
                    nest_depth: 0,
                    subquery_depth: 0,
                    captures: std::cell::RefCell::new(Vec::new()),
                    first_steps: std::cell::RefCell::new(Vec::new()),
                };
                let op = p.cond_expr().map_err(wrap)?;
                if p.pos != tokens.len() {
                    return Err(QuarbError::Parse(format!(
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
            return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
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
            return Err(QuarbError::Parse(format!(
                "fragment '&{name}' walks reverse resolution '<--', \
                 which is not allowed inside a predicate (it would scan \
                 the whole arbor per node); rewrite as a descending path \
                 or an incoming edge '<-'"
            )));
        }
        if q.pipeline.is_empty() {
            return Ok(rel);
        }
        for stage in &q.pipeline {
            if matches!(
                stage,
                Stage::Push(_)
                    | Stage::ExprPush { .. }
                    | Stage::RecordPush { .. }
                    | Stage::Subcontext { .. }
                    | Stage::Nav(_)
            ) {
                return Err(QuarbError::Parse(format!(
                    "fragment '&{name}' carries a pipeline that pushes or \
                     navigates; an operand pipe transforms a value \
                     (pushes belong to real capsae)"
                )));
            }
        }
        Ok(Operand::Piped {
            expr: Box::new(rel),
            stages: q.pipeline,
        })
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek()? {
            Token::Eq => CmpOp::Eq,
            Token::Ne => CmpOp::Ne,
            Token::Lt => CmpOp::Lt,
            Token::Le => CmpOp::Le,
            Token::Gt => CmpOp::Gt,
            Token::Ge => CmpOp::Ge,
            Token::Match => CmpOp::Match,
            Token::NotMatch => CmpOp::NotMatch,
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
        self.expect(Token::Colon, what)
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

    /// Consume the bare keyword `kw` if it is next.
    fn eat_keyword(&mut self, kw: &str) -> bool {
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && text == kw
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
            Err(QuarbError::Parse(format!("expected {what}")))
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
        if !matches!(self.peek(), Some(Token::Lt)) {
            return Ok(None);
        }
        let start = self.pos;
        self.pos += 1; // consume '<'
        let Some(expr) = self.trait_or() else {
            self.pos = start;
            return Ok(None);
        };
        if !matches!(self.peek(), Some(Token::Gt)) {
            self.pos = start;
            return Ok(None);
        }
        self.pos += 1;
        trait_cnf(expr).map(Some)
    }

    // The trait algebra takes the boolean words too (`<admin and
    // not banned>`, and their spellings in the family's languages —
    // ruling #42, completed): a word in operator position is the
    // operator; a lone word is still a trait name.
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
                let name = text.clone();
                self.pos += 1;
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
            Some(Token::Gt) | Some(Token::NextSibling) => Axis::NextSibling,
            Some(Token::Lt) | Some(Token::PrevSibling) => Axis::PrevSibling,
            Some(Token::FollowingSiblings(mark)) => Axis::FollowingSiblings(mark_reach(*mark)),
            Some(Token::PrecedingSiblings(mark)) => Axis::PrecedingSiblings(mark_reach(*mark)),
            Some(Token::ArrowOut) => Axis::OutLink,
            Some(Token::ArrowIn) => Axis::InLink,
            Some(Token::DashDash) => Axis::BothLink,
            Some(Token::Name { text, .. }) => {
                return Err(QuarbError::Parse(format!(
                    "expected a navigation operator before '{text}' \
                     (queries are root-anchored; start with '/', or open \
                     with an expression head: '= expr')"
                )));
            }
            // `:name` after a step: a node has properties, not
            // fields (ruling #48).
            Some(Token::Field) => {
                let name = match self.peek() {
                    Some(Token::Name { text, .. }) => text.clone(),
                    _ => "name".to_string(),
                };
                return Err(QuarbError::Parse(format!(
                    "':{name}' reads a record's field; a node's property is '::{name}'"
                )));
            }
            _ => {
                return Err(QuarbError::Parse(
                    "expected a navigation operator ('/', '//', '\\', …)".into(),
                ));
            }
        };
        Ok(axis)
    }

    /// Consume an optional `?` (proximal) or `!` (distal) suffix.
    fn reach(&mut self) -> Reach {
        match self.peek() {
            Some(Token::Question) => {
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

    fn matcher(&mut self) -> Result<Matcher> {
        let in_pattern = self.pattern_depth > 0;
        // `/<block>` is sugar for `/*<block>`: a trait block directly
        // after an axis matches any node, the traits filtering it.
        // Leave the `<` for finish_step's trait parser to consume.
        if matches!(self.peek(), Some(Token::Lt)) {
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
                .map_err(|e| QuarbError::Parse(format!("bad regex '~({pat})': {e}"))),
            _ => Err(QuarbError::Parse(
                "a navigation operator must be followed by a name".into(),
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
            return Err(QuarbError::Parse(format!(
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
                return Err(QuarbError::Parse(format!(
                    "decode: '{}' is not a decodable format \
                     (base64, base64url, base32, crockford32, hex, \
                      json, yaml, toml, xml)",
                    v
                )));
            }
            _ => {
                return Err(QuarbError::Parse(
                    "decode takes one scheme name, e.g. decode(base64)".into(),
                ));
            }
        }
    }
    if !matches!(call.name.as_str(), "record" | "rec") {
        return Ok(());
    }
    validate_record_convention(call, "record")
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
        return Err(QuarbError::Parse(format!(
            "{what} needs at least one {noun}, e.g. {what}(::name)"
        )));
    }
    // Field names are fully static — literals or auto-named
    // projections — so a collision is a parse error, never a
    // record with duplicate keys.
    let mut names: Vec<String> = Vec::new();
    let mut check = |name: &str| -> Result<()> {
        if names.iter().any(|n| n == name) {
            return Err(QuarbError::Parse(format!(
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
                    return Err(QuarbError::Parse(format!(
                        "{what} has a trailing field name with no value"
                    )));
                }
                check(name)?;
                i += 2;
            }
            Arg::Expr(e) => {
                let Some(name) = crate::ast::auto_field_name(e) else {
                    return Err(QuarbError::Parse(format!(
                        "a {what} field needs a name: give a computed value a key, \
                         e.g. %(total = ::price * ::qty)"
                    )));
                };
                let name = name.to_string();
                check(&name)?;
                i += 1;
            }
            _ => {
                return Err(QuarbError::Parse(format!(
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
/// keyed function needs at least one expression key, and `top` /
/// `bottom` take a literal integer count first.
fn validate_keyed(call: &FnCall) -> Result<()> {
    // `group` follows the record convention for its keys (auto-named
    // projections, or literal-name-then-expression).
    if call.name == "group" {
        return validate_record_convention(call, "group");
    }
    // `sort` takes at most one argument: a Unicode locale
    // identifier selecting the collation (`sort(ru-RU)`),
    // validated here so a typo fails the parse, not the sort.
    // The check is syntactic (BCP 47 shape) and deliberately
    // feature-independent: a query parses identically whether or
    // not collation is compiled in; existence and support are
    // the collator's concern at sort time.
    if call.name == "sort" {
        match call.args.as_slice() {
            [] => return Ok(()),
            [Arg::Lit(v)] => {
                let tag = v.to_string();
                if !valid_locale_tag(&tag) {
                    return Err(QuarbError::Parse(format!(
                        "sort: '{tag}' is not a Unicode locale identifier                          (try ru-RU, de-DE, zh-Hant, ...)"
                    )));
                }
                return Ok(());
            }
            _ => {
                return Err(QuarbError::Parse(
                    "sort takes at most one argument: a locale identifier,                      e.g. sort(ru-RU); keyed sorting is sort_by"
                        .into(),
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
        && !matches!(args.next(), Some(Arg::Lit(Value::Int(n))) if *n >= 0)
    {
        return Err(QuarbError::Parse(format!(
            "{} takes a non-negative integer count first: {}(3, ::key)",
            call.name, call.name
        )));
    }
    let mut keys = args.peekable();
    if keys.peek().is_none() || keys.any(|a| matches!(a, Arg::Lit(_))) {
        return Err(QuarbError::Parse(format!(
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
                    return Err(QuarbError::Parse(format!(
                        "window({a}..{b}) is empty: the range needs start <= end"
                    )));
                }
                Ok(())
            }
            Some((Arg::Lit(Value::Int(n)), rest)) if *n >= 1 && key_ok(rest) => Ok(()),
            _ => Err(QuarbError::Parse(
                "window takes an offset range or a count, then an optional \
                 partition key: window(-2..0), window(3, ::group)"
                    .into(),
            )),
        },
        "shift" => match call.args.split_first() {
            Some((Arg::Lit(Value::Int(_)), rest)) if key_ok(rest) => Ok(()),
            _ => Err(QuarbError::Parse(
                "shift takes an integer distance, then an optional partition \
                 key: shift(1), shift(1, ::group)"
                    .into(),
            )),
        },
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
            Glob::new(text).map_err(|e| QuarbError::Parse(format!("bad glob '{text}': {e}")))?;
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
        for elem in &mut b.steps {
            subst_elem(elem, map);
        }
    }
    for stage in &mut q.pipeline {
        subst_stage(stage, map);
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

fn subst_stage(stage: &mut Stage, map: &Subst<'_>) {
    match stage {
        Stage::Func(call) | Stage::Agg(call) | Stage::RecordWith(call) | Stage::RecordPush { call, .. } => {
            for arg in &mut call.args {
                if let Arg::Expr(e) = arg {
                    subst_operand(e, map);
                }
            }
        }
        Stage::Expr(e) | Stage::ExprPush { expr: e, .. } => subst_operand(e, map),
        Stage::Nav(b) => {
            for elem in &mut b.steps {
                subst_elem(elem, map);
            }
        }
        Stage::Subcontext { body, .. } => subst_query(body, map),
        Stage::Filter(e) => subst_pred_expr(e, map),
        Stage::Select(Predicate::Expr(e)) => subst_pred_expr(e, map),
        Stage::Map(inner) => subst_stage(inner, map),
        Stage::Select(_) | Stage::Push(_) | Stage::Recall(_) | Stage::Spread { .. } => {}
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
        Operand::Rel { steps, .. } | Operand::Ctx { steps, .. } => {
            for elem in steps {
                subst_elem(elem, map);
            }
        }
        Operand::Arith { left, right, .. } => {
            subst_operand(left, map);
            subst_operand(right, map);
        }
        Operand::Neg(inner) => subst_operand(inner, map),
        Operand::Field { base, .. } => subst_operand(base, map),
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
        Operand::Piped { expr, stages } => {
            subst_operand(expr, map);
            for st in stages.iter_mut() {
                subst_stage(st, map);
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
        | Operand::Ordinal
        | Operand::Edge { .. }
        | Operand::Edges { .. }
        | Operand::Capsae { .. }
        | Operand::Capture(_)
        | Operand::Now => {}
    }
}

/// The largest `$*k` index mentioned under an AST piece — 0 when
/// none. Backs the driver-first placement rules: correlated
/// references live on a joined expression's final step (its ON
/// clause) and in the driver's pipeline, nowhere else.
fn max_ctx_query(q: &Query) -> usize {
    q.correlations
        .iter()
        .map(max_ctx_query)
        .chain(q.branches.iter().flat_map(|b| b.steps.iter().map(max_ctx_elem)))
        .chain(q.pipeline.iter().map(max_ctx_stage))
        .max()
        .unwrap_or(0)
}

fn max_ctx_elem(elem: &PathElem) -> usize {
    match elem {
        PathElem::Mark(_) => 0,
        PathElem::Step(s) => max_ctx_step(s),
        PathElem::Group(g) => g
            .alts
            .iter()
            .flat_map(|alt| alt.iter().map(max_ctx_elem))
            .chain(g.predicates.iter().map(max_ctx_pred))
            .max()
            .unwrap_or(0),
        PathElem::Push { body, .. } => match body {
            PushBody::Query(q) => max_ctx_query(q),
            PushBody::Expr(e) => max_ctx_operand(e),
        },
    }
}

fn max_ctx_step(s: &Step) -> usize {
    s.predicates.iter().map(max_ctx_pred).max().unwrap_or(0)
}

fn max_ctx_pred(p: &Predicate) -> usize {
    match p {
        Predicate::Expr(e) => max_ctx_pred_expr(e),
        Predicate::Index(_) | Predicate::Range(_, _) => 0,
    }
}

pub(crate) fn max_ctx_pred_expr(e: &PredExpr) -> usize {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => max_ctx_pred_expr(a).max(max_ctx_pred_expr(b)),
        PredExpr::Not(a) => max_ctx_pred_expr(a),
        PredExpr::Compare(l, _, r) => max_ctx_operand(l).max(max_ctx_operand(r)),
        PredExpr::Truthy(o) => max_ctx_operand(o),
    }
}

fn max_ctx_stage(st: &Stage) -> usize {
    match st {
        Stage::Func(call) | Stage::Agg(call) | Stage::RecordWith(call) | Stage::RecordPush { call, .. } => call
            .args
            .iter()
            .map(|a| match a {
                Arg::Expr(e) => max_ctx_operand(e),
                _ => 0,
            })
            .max()
            .unwrap_or(0),
        Stage::Expr(e) | Stage::ExprPush { expr: e, .. } => max_ctx_operand(e),
        Stage::Nav(b) => b.steps.iter().map(max_ctx_elem).max().unwrap_or(0),
        Stage::Subcontext { body, .. } => max_ctx_query(body),
        Stage::Filter(e) => max_ctx_pred_expr(e),
        Stage::Select(p) => max_ctx_pred(p),
        Stage::Map(inner) => max_ctx_stage(inner),
        Stage::Push(_) | Stage::Recall(_) | Stage::Spread { .. } => 0,
    }
}

fn max_ctx_operand(o: &Operand) -> usize {
    match o {
        Operand::Ctx { index, steps, .. } => index
            .unwrap_or(0)
            .max(steps.iter().map(max_ctx_elem).max().unwrap_or(0)),
        Operand::Rel { steps, .. } => steps.iter().map(max_ctx_elem).max().unwrap_or(0),
        Operand::Arith { left, right, .. } => max_ctx_operand(left).max(max_ctx_operand(right)),
        Operand::Neg(inner) | Operand::Outer(inner) => max_ctx_operand(inner),
        Operand::Group(e) => max_ctx_pred_expr(e),
        Operand::Piped { expr, stages } => max_ctx_operand(expr)
            .max(stages.iter().map(max_ctx_stage).max().unwrap_or(0)),
        Operand::Cond { cond, then, other } => max_ctx_pred_expr(cond)
            .max(max_ctx_operand(then))
            .max(max_ctx_operand(other)),
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => max_ctx_operand(scrutinee)
            .max(
                arms.iter()
                    .map(|(t, _, r)| max_ctx_operand(t).max(max_ctx_operand(r)))
                    .max()
                    .unwrap_or(0),
            )
            .max(max_ctx_operand(other)),
        Operand::Interp(segs) => segs
            .iter()
            .map(|seg| match seg {
                InterpSeg::Expr(e) | InterpSeg::Strict(e, _) => max_ctx_operand(e),
                InterpSeg::Default(e, f) => max_ctx_operand(e).max(max_ctx_operand(f)),
                InterpSeg::Text(_) => 0,
            })
            .max()
            .unwrap_or(0),
        _ => 0,
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
/// stage before it, it would rank each capsa's single value against
/// itself and pass everything through — refuse with the pointer.
fn validate_keyed_stages(q: &Query) -> Result<()> {
    let mut grouped = false;
    for stage in &q.pipeline {
        match stage {
            // `@| group` and `@| window` partition the stream; a
            // keyed stage on the plain pipe then works per part.
            Stage::Agg(call) if matches!(call.name.as_str(), "group" | "window") => grouped = true,
            Stage::Func(call) if crate::stdlib::known_keyed(&call.name) && !grouped => {
                return Err(QuarbError::Parse(format!(
                    "'{n}' ranks a context: write '@| {n}(…)' (on the plain pipe it works per group, after '@| group')",
                    n = call.name
                )));
            }
            _ => {}
        }
    }
    for c in &q.correlations {
        validate_keyed_stages(c)?;
    }
    Ok(())
}

fn validate_correlation_refs(q: &Query) -> Result<()> {
    let n = q.correlations.len();
    for b in &q.branches {
        for elem in &b.steps {
            if max_ctx_elem(elem) > 0 {
                return Err(QuarbError::Parse(
                    "the first expression drives the join and cannot \
                     reference '$*k'; join conditions belong on the \
                     joined expression: 'A <=> B[::x = $$::x]'"
                        .into(),
                ));
            }
        }
    }
    for st in &q.pipeline {
        let m = max_ctx_stage(st);
        if m > n {
            return Err(QuarbError::Parse(format!(
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
                    return Err(QuarbError::Parse(
                        "join conditions belong in the joined \
                         expression's final step's brackets"
                            .into(),
                    ));
                }
                if m >= k {
                    return Err(QuarbError::Parse(format!(
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
                            return Err(QuarbError::Parse(
                                "positional selection after a join condition \
                                 is ambiguous — select before the join \
                                 condition, or in the pipeline"
                                    .into(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        for st in &entry.pipeline {
            if max_ctx_stage(st) > 0 {
                return Err(QuarbError::Parse(
                    "a joined expression's own pipeline shapes its context \
                     before the join and cannot reference '$*k'; join \
                     conditions belong in its final step's brackets"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

/// Whether a predicate expression mentions the invoking capsa
/// (`$$…`) anywhere — the other half of an ON clause.
pub(crate) fn pred_mentions_outer(e: &PredExpr) -> bool {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            pred_mentions_outer(a) || pred_mentions_outer(b)
        }
        PredExpr::Not(a) => pred_mentions_outer(a),
        PredExpr::Compare(l, _, r) => op_mentions_outer(l) || op_mentions_outer(r),
        PredExpr::Truthy(o) => op_mentions_outer(o),
    }
}

fn op_mentions_outer(o: &Operand) -> bool {
    match o {
        Operand::Outer(_) => true,
        Operand::Ctx { steps, .. } | Operand::Rel { steps, .. } => {
            steps.iter().any(elem_mentions_outer)
        }
        Operand::Arith { left, right, .. } => {
            op_mentions_outer(left) || op_mentions_outer(right)
        }
        Operand::Neg(inner) => op_mentions_outer(inner),
        Operand::Group(e) => pred_mentions_outer(e),
        Operand::Piped { expr, stages } => {
            op_mentions_outer(expr) || stages.iter().any(stage_mentions_outer)
        }
        Operand::Cond { cond, then, other } => {
            pred_mentions_outer(cond)
                || op_mentions_outer(then)
                || op_mentions_outer(other)
        }
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            op_mentions_outer(scrutinee)
                || arms
                    .iter()
                    .any(|(t, _, r)| op_mentions_outer(t) || op_mentions_outer(r))
                || op_mentions_outer(other)
        }
        Operand::Interp(segs) => segs.iter().any(|seg| match seg {
            InterpSeg::Expr(e) | InterpSeg::Strict(e, _) => op_mentions_outer(e),
            InterpSeg::Default(e, f) => op_mentions_outer(e) || op_mentions_outer(f),
            InterpSeg::Text(_) => false,
        }),
        _ => false,
    }
}

pub(crate) fn elem_mentions_outer(e: &PathElem) -> bool {
    match e {
        PathElem::Mark(_) => false,
        PathElem::Step(s) => s.predicates.iter().any(|p| match p {
            Predicate::Expr(e) => pred_mentions_outer(e),
            _ => false,
        }),
        PathElem::Group(g) => {
            g.alts.iter().any(|alt| alt.iter().any(elem_mentions_outer))
                || g.predicates.iter().any(|p| match p {
                    Predicate::Expr(e) => pred_mentions_outer(e),
                    _ => false,
                })
        }
        PathElem::Push { body, .. } => match body {
            PushBody::Query(q) => query_mentions_outer(q),
            PushBody::Expr(e) => op_mentions_outer(e),
        },
    }
}

fn stage_mentions_outer(st: &Stage) -> bool {
    match st {
        Stage::Func(call) | Stage::Agg(call) => call.args.iter().any(|a| match a {
            Arg::Expr(e) => op_mentions_outer(e),
            _ => false,
        }),
        Stage::Expr(e) | Stage::ExprPush { expr: e, .. } => op_mentions_outer(e),
        Stage::Nav(b) => b.steps.iter().any(elem_mentions_outer),
        Stage::Subcontext { body, .. } => query_mentions_outer(body),
        Stage::Filter(e) => pred_mentions_outer(e),
        Stage::Select(Predicate::Expr(e)) => pred_mentions_outer(e),
        Stage::Map(inner) => stage_mentions_outer(inner),
        _ => false,
    }
}

fn query_mentions_outer(q: &Query) -> bool {
    q.branches
        .iter()
        .any(|b| b.steps.iter().any(elem_mentions_outer))
        || q.pipeline.iter().any(stage_mentions_outer)
        || q.correlations.iter().any(query_mentions_outer)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let pairs: Vec<String> = (0..24).map(|i| format!("(a{i}&&b{i})")).collect();
        let toks = lexer::lex(&format!("//*<{}>", pairs.join("||"))).unwrap();
        assert!(parse(&toks).is_err());
        // A modest algebra still normalizes.
        let toks = lexer::lex("//*<(a&&b)||(c&&d)>").unwrap();
        assert!(parse(&toks).is_ok());
    }

    #[test]
    fn record_field_names_must_be_unique() {
        for q in [
            "/r/* | rec(::name, ::name)",
            "/r/* | rec(\"a\", ::x, \"a\", ::y)",
            "/r/* | rec(\"name\", ::x, ::name)",
            "/r/* @| group(::k, ::k) | count",
        ] {
            let toks = lexer::lex(q).unwrap();
            let err = parse(&toks).expect_err(q).to_string();
            assert!(err.contains("twice"), "{q}: {err}");
        }
        // Distinct names stay fine, literal or auto-named.
        let toks = lexer::lex("/r/* | rec(::name, \"other\", ::name)").unwrap();
        assert!(parse(&toks).is_ok());
    }

    #[test]
    fn fragment_bodies_cover_chains_and_inline_pipes() {
        // A def may name a whole correlation; invoking it stands
        // alone, takes a pipe tail, and extends with further
        // entries.
        for q in [
            "def &j: /a/* <=> /b/*[::x = $$::x]; &j @| count",
            "def &j: /a/* <=> /b/*[::x = $$::x]; &j | [not $*1::x]",
            "def &j: /a/* <=> /b/*[::x = $$::x]; &j <=> /c/*[::y = $*1::y]",
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
                "def &j: /a/* <=> /b/*[::x = $$::x]; /d/* <=> &j",
                "chains are flat",
            ),
            // a chain fragment cannot join a union
            (
                "def &j: /a/* <=> /b/*[::x = $$::x]; /d/* || &j",
                "stand alone",
            ),
            // a stage-shaped def body points at the leading pipe
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
        // `%%.` parses as its own recall; `%.` stays itself — and
        // numeric push names stay legal (the pivot macro writes
        // data-valued column names like `.1(...)`); `%%.` keys
        // anonymous slots `#N`, which no push name can spell.
        for q in [
            "/r/* | .(::a) | %%.",
            "/r/* | .x(::a) | %. | %%.",
            "/r/* | .1(::a) | %.",
        ] {
            let toks = lexer::lex(q).unwrap();
            assert!(parse(&toks).is_ok(), "should parse: {q}");
        }
    }

    #[test]
    fn driver_first_placement_rules() {
        // Legal: ON on the joined expression ($$ = driver, $*k =
        // earlier entries), witnesses read back in the pipeline.
        for q in [
            "/a/* <=> /b/*[::x = $$::x]",
            "/a/* <=>? /b/*[::x = $$::x] | [not $*1::x]",
            "/a/* <=> /b/*[::x = $$::x] <=> /c/*[::y = $*1::y] | rec($*1::n, $*2::m)",
            "/a/* <=> /b/*[$$/kid::x = ::x]",
        ] {
            let toks = lexer::lex(q).unwrap();
            assert!(parse(&toks).is_ok(), "should parse: {q}");
        }
        // The driver cannot reference the join from its own steps.
        for (q, needle) in [
            ("/a/*[::x = $*1::x] <=> /b/*", "first expression drives"),
            // A joined expression cannot reference itself or later
            // entries.
            ("/a/* <=> /b/*[::x = $*1::x]", "joined expression #1"),
            ("/a/* <=> /b/*[::x = $*2::x] <=> /c/*", "joined expression #1"),
            // The pipeline cannot outrun the join count.
            ("/a/* <=> /b/* | rec($*2::n)", "only 1 expression"),
            // Join conditions sit on the final step.
            ("/a/* <=> /b[$$::x]/c/*", "final step"),
            // Positional selection after an ON clause is ambiguous.
            (
                "/a/* <=> /b/*[::x = $$::x][1]",
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
        // A push body, a function argument, and a fragment argument
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
            // function arguments: rec named field, group key
            "/r/* | rec(\"age\", ::Age ? ::Age * 1 : 0)",
            "/r/* @| group(\"src\", ::Age ? \"manifest\" : \"records\") | count",
            // fragment argument
            "def &f($x): /r/*[::a = $x]; &f(::b ? 1 : 2)",
            // the old double-parens spelling must keep parsing
            "/r/* | .born((::quarter ? 1890 : 0))",
            "/r/* | rec(\"age\", (( ::Age ? 1 : 0 )))",
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
    fn bare_inf_and_nan_are_string_literals() {
        // Rust's f64 parser accepts the words; the query language
        // does not — a bare word is text.
        let toks = lexer::lex("/x[::status = inf]").unwrap();
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
            q.pipeline.iter().any(|s| matches!(s, Stage::Map(_))),
            "expected a map stage, got {:?}",
            q.pipeline
        );
        assert!(
            !last_step(&q).leaf,
            "the step preceding `$|` must not be leaf-anchored"
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
        // A macro body's shell stage is evaluated at expansion (parse)
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
        assert!(parse(&lexer::lex("//a[::r<~]").unwrap()).is_err());
        assert!(parse(&lexer::lex("//a::r<~").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//a[<-b]").unwrap()).is_ok());
    }
}


/// Find a top-level bash hole operator (ruling #34): a `:` followed
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
        ".minor." | ".inf." | ".менее." | ".μικρότερο." | ".μικροτερο." => CmpOp::Lt,
        ".nonmaior." | ".nonsup." | ".неболее." | ".τοπολύ." | ".τοπολυ." => CmpOp::Le,
        ".maior." | ".sup." | ".более." | ".μεγαλύτερο." | ".μεγαλυτερο." => CmpOp::Gt,
        ".nonminor." | ".noninf." | ".неменее." | ".τουλάχιστον." | ".τουλαχιστον." => CmpOp::Ge,
        _ => return None,
    })
}

/// The boolean words — `and` / `or` / `not`, the parsed aliases of
/// `&&` / `||` / `!` — in each language of the rounded family
/// (ruling #42): Latin (`vel`, the inclusive or), French, Russian,
/// Greek with or without the tonos. On layouts without `&` and `|`
/// the words are the only spelling of conjunction and disjunction.
const AND_WORDS: &[&str] = &["and", "et", "и", "και"];
const OR_WORDS: &[&str] = &["or", "vel", "ou", "или", "ή", "η"];
const NOT_WORDS: &[&str] = &["not", "non", "не", "όχι", "οχι"];

/// Whether a bare word is one of the boolean words — which, unquoted,
/// is never a property name.
pub(crate) fn is_bool_word(word: &str) -> bool {
    AND_WORDS.contains(&word) || OR_WORDS.contains(&word) || NOT_WORDS.contains(&word)
}
