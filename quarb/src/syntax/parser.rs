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

use crate::error::{QuarbError, Result};
use crate::syntax::ast::visit::{self, Visitor as _};
use crate::syntax::ast::{
    Anchor, Arg, ArithOp, Axis, Branch, CmpOp, Command, ConcatMode, FnCall, Group, IndexSpec,
    InterpSeg, LabelAlt, LabelClause, Matcher, Operand, PatSeg, PathElem, PredExpr, Predicate,
    ProjShape, Projection, PushBody, Quant, Query, Reach, RefProp, RegRef, ScopePath, Step,
    TableRow, TraitClause,
};
use crate::syntax::expand::{Expander, MacroBinding, RegUsage};
use crate::syntax::lexer::{self, Token};
use crate::value::ColumnType;
use crate::value::Value;
use crate::value::{AND_WORDS, NOT_WORDS, OR_WORDS, is_bool_word};
use globset::Glob;
use regex::Regex;
use std::collections::HashMap;

/// Parse a token stream into a [`Query`], expanding any inline
/// `def` statements.
/// Parse a scope path on its own (`/orders/*`, `//a[::x]`) into the
/// shape the arrow's argument carries, so a declaration naming a
/// scope compares with a query's by canonical text.
pub fn parse_scope_path(text: &str) -> Result<ScopePath> {
    let toks = crate::syntax::lexer::lex(text)?;
    let expander = crate::graph::reflect::Reflector::new(None);
    let mut p = Parser::new(&toks, Defs::default(), &expander);
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
    parse_with_data(tokens, defs, &crate::graph::reflect::Reflector::new(None))
}

/// Parse with a fragment table and the dataset being queried; the
/// dataset is what a data-aware macro (`&name!`) reads at expansion
/// time, mounted as `/data` in its expansion arbor.
pub fn parse_with_data(tokens: &[Token], defs: Defs, expander: &dyn Expander) -> Result<Query> {
    parse_with_data_spanned(tokens, None, defs, expander)
}

/// [`parse_with_data`] with the tokens' spans (character offsets
/// from the lexer, `lex_spanned`): a parse error then says where
/// in the query it stopped.
pub fn parse_with_data_spanned(
    tokens: &[Token],
    spans: Option<&[(usize, usize)]>,
    defs: Defs,
    expander: &dyn Expander,
) -> Result<Query> {
    let mut p = Parser::new(tokens, defs, expander);
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
    expander: &dyn Expander,
) -> Result<Vec<String>> {
    let mut p = Parser::new(tokens, defs, expander);
    p.parse()?;
    Ok(p.first_steps.into_inner())
}

/// Parse a token stream containing only `def` statements into a
/// fragment table (the `--defs` file format).
pub fn parse_defs(tokens: &[Token]) -> Result<Defs> {
    let expander = crate::graph::reflect::Reflector::new(None);
    let mut p = Parser::new(tokens, Defs::default(), &expander);
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

/// Whether `tokens` are definitions and nothing else: the leading
/// `def`/`macro` statements parse (a malformed one is the error),
/// and `Ok(false)` says a query follows them — text the query path
/// reads whole, leading definitions included. Session layers use it
/// to tell a definitions cell from a query cell with a preamble.
pub fn defs_only(tokens: &[Token]) -> Result<bool> {
    let expander = crate::graph::reflect::Reflector::new(None);
    let mut p = Parser::new(tokens, Defs::default(), &expander);
    while p.at_def() || p.at_macro() {
        if p.at_def() {
            p.parse_def()?;
        } else {
            p.parse_macro()?;
        }
    }
    Ok(p.peek().is_none())
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
    /// The body names `$$0` (directly, or through a fragment it
    /// invokes). Its placement is judged where the expansion lands:
    /// a join condition admits it, anywhere else the invocation is
    /// the parse error.
    uses_driver: bool,
    /// The unit operands the body spells (`convert(U)`, `unit(U)`,
    /// `magnitude(U)`: the command and the operand), when the body
    /// parsed without a dataset to judge them by (a definitions
    /// file); the splice site judges them (spec: The conversion).
    unit_operands: Vec<(String, String)>,
    body: DefBody,
}

/// The value-side capsa read after `$.` (spec: The Capsa): `$.` the
/// latest regula, `$.N` the N-th, `$.-N` the N-th back from the
/// latest, `$..` the penultimate, `$.name` the latest under a name;
/// any other shape is refused with the forms.
fn reg_ref(rest: &str) -> Result<RegRef> {
    if rest.is_empty() {
        return Ok(RegRef::Top);
    }
    if rest == "." {
        return Ok(RegRef::Back(2));
    }
    if let Some(d) = rest.strip_prefix('-') {
        if let Ok(n) = d.parse::<usize>()
            && n >= 1
            && d.bytes().all(|c| c.is_ascii_digit())
        {
            return Ok(RegRef::Back(n));
        }
    } else if rest.bytes().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = rest.parse::<usize>()
            && n >= 1
        {
            return Ok(RegRef::Index(n));
        }
    } else if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && rest
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(RegRef::Named(rest.to_string()));
    }
    Err(QuarbError::parse(format!(
        "'$.{rest}' is no capsa read: '$.' is the latest value regula, '$.N' the N-th, \
         '$.-N' the N-th back from the latest, '$..' the penultimate, '$.name' the latest \
         under a name"
    )))
}

/// A node projection written on the cursor, as the parse error says
/// it: the cursor is a record, its position the node.
const CURSOR_PROJECTION: &str = "'_' is the cursor: the position's property is '_:position::k' or '$$_::k'; the driver of a join is '$$0::k'";

/// The driver's placement rule, as the parse error says it.
const DRIVER_PLACEMENT: &str = "'$$0' names the driver of a correlation inside its \
                                join condition; elsewhere the invoking position is \
                                '$$__' and the position '$$_'";

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
        | Command::AggExpr(_)
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
fn invites_of(expander: &dyn Expander, args: &[Operand]) -> std::collections::BTreeSet<String> {
    let mut s = std::collections::BTreeSet::new();
    for a in args {
        s.extend(expander.operand_invites(a));
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
    /// What expands a macro and reads a fragment's register usage:
    /// the layer that can evaluate, over the dataset when there is one.
    expander: &'a dyn Expander,
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
    /// How many correlations enclose the cursor: `$$0` names the
    /// driver only inside a join condition.
    correlation_depth: usize,
    /// Inside a fragment body: `$$0` is admitted here and judged at
    /// the splice site instead (see [`Def::uses_driver`]).
    in_def_body: bool,
    /// Set while a fragment body parses when `$$0` appears outside
    /// any join of the body's own, directly or through a fragment
    /// the body invokes.
    body_uses_driver: bool,
    /// The unit operands a fragment body spelled that no dataset
    /// could judge yet (see [`Def::unit_operands`]).
    body_unit_operands: Vec<(String, String)>,
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
    /// Inside a keyed aggregate's parentheses (`sort_by(…)`): the
    /// rounded `-:col` there is the descending core-field key.
    keyed_args: bool,
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
        // the session push leaves the cursor untouched, as a push does
        Command::SessionPush(_) => cur,
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
        // a spliced quarq may navigate: the permissive mode follows
        Command::Splice(_) => PipeMode::Nav,
        Command::Func(_)
        | Command::Expr(_)
        | Command::Agg(_)
        | Command::AggRecord { .. }
        | Command::AggExpr(_)
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
    fn new(toks: &'a [Token], defs: Defs, expander: &'a dyn Expander) -> Parser<'a> {
        Parser {
            toks,
            pos: 0,
            defs,
            def_params: Vec::new(),
            expander,
            pattern_depth: 0,
            predicate_depth: 0,
            correlation_depth: 0,
            in_def_body: false,
            body_uses_driver: false,
            body_unit_operands: Vec::new(),
            nest_depth: 0,
            subquery_depth: 0,
            arg_depth: 0,
            keyed_args: false,
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
            in_def_body: self.in_def_body,
            ..Parser::new(toks, defs, self.expander)
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
            // `:name` spaced after a path: a core field glues to
            // its step.
            if matches!(tok, Token::Field)
                && let Some(Token::Name { text, .. }) = self.toks.get(self.pos + 1)
            {
                return Err(QuarbError::parse(format!(
                    "':{text}' stands apart from its path: a core field glues to the step \
                     ('/x:{text}'; ':fields' lists them), a property is '::{text}'"
                )));
            }
            // `= 12 h`, `| 12 km`: a literal head continues only with
            // an operator; the spellings a reader wants are the
            // quantity and the text.
            if self.pos > 0
                && let Some(Token::Name {
                    text: prev,
                    quoted: false,
                    ..
                }) = self.toks.get(self.pos - 1)
                && let Token::Name {
                    text: word,
                    quoted: false,
                    ..
                } = tok
                && is_word(word)
                && matches!(
                    literal_value(prev, false, &|e| self.expander.unit_scale(e)),
                    Value::Int(_) | Value::Float(_)
                )
            {
                return Err(QuarbError::parse(format!(
                    "'{prev} {word}' is no expression: a literal head continues only with an \
                     operator — write {prev}_{word} for the quantity, \"{prev} {word}\" for the text"
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
            self.correlation_depth += 1;
            let entry = self.parse_query();
            self.correlation_depth -= 1;
            let mut entry = entry?;
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
        // `session(--pretty=always)` / `session` at the head (spec:
        // Session settings, draft 166): the session command on the
        // root, a pipe may follow as after `=`
        if branches.is_empty()
            && self.subquery_depth == 0
            && matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "session")
            && matches!(
                self.toks.get(self.pos + 1),
                None | Some(Token::LParen | Token::Pipe)
            )
        {
            self.pos += 1;
            let mut args: Vec<Arg> = Vec::new();
            if matches!(self.peek(), Some(Token::LParen)) {
                self.pos += 1;
                loop {
                    if matches!(self.peek(), Some(Token::RParen)) {
                        self.pos += 1;
                        break;
                    }
                    let Some(name) = self.option_item() else {
                        return Err(QuarbError::parse(
                            "'session(…)' takes its settings as options: 'session(--pretty=always)', 'session(--pretty=structures)', 'session(--pretty=off)', 'session(--precision=n)'",
                        ));
                    };
                    match (self.peek(), self.toks.get(self.pos + 1)) {
                        (Some(Token::DashDash), Some(Token::Name { glued: true, .. })) => {
                            self.pos += 2
                        }
                        _ => self.pos += 1,
                    }
                    let value = if matches!(self.peek(), Some(Token::OptEq)) {
                        self.pos += 1;
                        match self.peek() {
                            Some(Token::Name { text, .. }) => {
                                let text = text.clone();
                                let v = match literal_value(&text, false, &|e| {
                                    self.expander.unit_scale(e)
                                }) {
                                    v @ Value::Int(_) => v,
                                    _ => Value::Str(text),
                                };
                                self.pos += 1;
                                Some(Operand::Lit(v))
                            }
                            _ => {
                                return Err(QuarbError::parse(format!(
                                    "'session({name}=…)' takes a word or a number"
                                )));
                            }
                        }
                    } else {
                        None
                    };
                    args.push(Arg::Opt { name, value });
                    match self.peek() {
                        Some(Token::Comma | Token::Semi) => self.pos += 1,
                        Some(Token::RParen) => {}
                        _ => {
                            return Err(QuarbError::parse(
                                "'session(…)' separates its settings with ';'",
                            ));
                        }
                    }
                }
            }
            branches.push(Branch {
                steps: Vec::new(),
                projection: None,
                anchor: Anchor::Root,
            });
            pipeline.push(Command::Func(FnCall {
                name: "session".into(),
                args,
            }));
            return Ok(());
        }
        if branches.is_empty()
            && self.subquery_depth == 0
            && (matches!(self.peek(), Some(Token::Eq)) || self.value_only_head())
        {
            // the sigil may be dropped before a token that can only
            // open a value (draft 166); the unparser prints it always
            if matches!(self.peek(), Some(Token::Eq)) {
                self.pos += 1;
                // the expression may be left out (draft 171): the
                // command after `=` is the first of the pipeline, the
                // root thread opened with no topic — `= .f(0) | …` is
                // `^ | .f(0) | …`. A pipe in its place stays a fragment.
                if matches!(self.peek(), Some(Token::Pipe)) {
                    return Err(QuarbError::parse(
                        "the head's expression may be left out, not replaced by a \
                         pipe: write '= .f(0) | …' (a leading pipe is a fragment)",
                    ));
                }
                if self.command_head_ahead() {
                    let command = self.pipe_item(PipeMode::Nav)?;
                    let command = self.quantified_push(command)?;
                    branches.push(Branch {
                        steps: Vec::new(),
                        projection: None,
                        anchor: Anchor::Root,
                    });
                    pipeline.push(command);
                    return Ok(());
                }
            }
            // the full expression: a comparison and a bare conditional
            // stand at the head (`= 1 = 1 ? "yes" : "no"`, draft 166);
            // a `||` after the operand is the union the head refuses
            let left = self.additive()?;
            if matches!(self.peek(), Some(Token::PipePipe)) || self.peek_word(OR_WORDS) {
                return Err(QuarbError::parse(
                    "an expression head stands alone; it cannot join a \
                     union — pipe from it instead: '= expr | …'",
                ));
            }
            let mut expr = self.expr_head_from(left)?;
            // the head has no cursor: a bare projection there, of
            // any rung, is a projector (draft 166), so
            // `= %(::Name; ::Fare)` stores two property projections
            head_projections(&mut expr);
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
                Some(Token::ColonColon | Token::ColonColonColon)
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
            let b = self.branch()?;
            // `//x:%(:props...)`, `//x:$.cols` — the projection
            // operator on a path at the query head: the path is the
            // operand, the query an expression head on each node.
            if branches.is_empty() && matches!(self.peek(), Some(Token::ProjectColon)) {
                self.pos += 1;
                let shape = self.project_shape()?;
                let base = Operand::Rel {
                    steps: b.steps,
                    projection: b.projection,
                    anchor: b.anchor,
                };
                branches.push(Branch {
                    steps: Vec::new(),
                    projection: None,
                    anchor: Anchor::Root,
                });
                pipeline.push(Command::Expr(Operand::Projected {
                    base: Box::new(base),
                    shape,
                }));
                return Ok(());
            }
            branches.push(b);
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
                | Token::ArrowIn { .. }
                | Token::DashDash
                | Token::LParen
                | Token::ColonColon
                | Token::ColonColonColon
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
                    let command = self.pipe_item(mode)?;
                    let command = self.quantified_push(command)?;
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
                            pred @ (Predicate::Index(_)
                            | Predicate::Range(_, _)
                            | Predicate::Positions(_)
                            | Predicate::Computed(_)) => {
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
                    // A literal or a constructor after `@|` opens an
                    // expression over the context as a whole (spec:
                    // Literals and constructors): `@| @(0; $_)` builds
                    // one list as `@| %(…)` builds one record, `@| 0`
                    // is one value; `#(--width=n)` stays the reshape.
                    if self.expr_head_ahead() {
                        let command = Command::AggExpr(self.additive()?);
                        mode = command_mode_out(&command, mode);
                        pipeline.push(command);
                        continue;
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
        // A binary arithmetic operator standing where a command
        // stands takes the topic as its left operand: `| * 2` is
        // `| ($_ * 2)`, and the rest of the expression follows at
        // the operator's own level (`| * 2 + 1` is `$_ * 2 + 1`).
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && let Some(op) = arith_word(text)
            && !matches!(
                self.toks.get(self.pos + 1),
                None | Some(Token::Pipe | Token::RParen)
            )
            && !(matches!(self.toks.get(self.pos + 1), Some(Token::At | Token::Dollar))
                && matches!(self.toks.get(self.pos + 2), Some(Token::Pipe)))
        {
            self.pos += 1;
            let left = match op {
                ArithOp::Add | ArithOp::Sub => {
                    let right = self.multiplicative()?;
                    Operand::Arith {
                        op,
                        left: Box::new(Operand::Topic),
                        right: Box::new(right),
                    }
                }
                _ => {
                    let right = self.unary()?;
                    self.multiplicative_from(Operand::Arith {
                        op,
                        left: Box::new(Operand::Topic),
                        right: Box::new(right),
                    })?
                }
            };
            return Ok(Command::Expr(self.additive_from(left)?));
        }
        // `| ++ x`, `| ++! x`, `| +@+ x` — a concatenation operator
        // after a pipe takes the topic as its left side like every
        // binary operator (spec: Concatenation, "After a pipe").
        if let Some(mode) = self.concat_word_ahead() {
            let left = self.concat_rest(mode, Operand::Topic)?;
            return Ok(Command::Expr(self.additive_from(left)?));
        }
        // `| ?? 0`, `| > 3 ? a : b` — the null default and the
        // comparisons after a pipe take the topic as their left
        // operand likewise (draft 166).
        if matches!(self.peek(), Some(Token::Coalesce)) {
            let cond = self.coalesce_tail(PredExpr::Truthy(Operand::Topic))?;
            return Ok(Command::Expr(self.cond_expr_from(cond)?));
        }
        if self.cmp_ahead() {
            let first = self.pred_primary_from(Operand::Topic)?;
            let first = self.pred_and_from(first)?;
            let cond = self.pred_or_from(first)?;
            let cond = self.coalesce_tail(cond)?;
            return Ok(Command::Expr(self.cond_expr_from(cond)?));
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
            Some(Token::Dollar) => Ok(Command::Expr(self.expr_head()?)),
            // `| _::prop` / `| _/kid::x` — the invoking position as the
            // command's expression.
            Some(Token::Driver | Token::Position | Token::Invoking) => {
                Ok(Command::Expr(self.expr_head()?))
            }
            // `| :name` — the topic record's field; `| %+` — the
            // named-captures record.
            Some(Token::Field | Token::PercentPlus) => Ok(Command::Expr(self.expr_head()?)),
            // `| @_`, `| @@_::prop`, `| #*`, `| ##*`, `| #_` — the
            // context family as the command's expression; `| _` the
            // cursor record.
            Some(
                Token::Topics
                | Token::Positions
                | Token::Matrix { .. }
                | Token::NodeMatrix { .. }
                | Token::CtxTable
                | Token::Cursor,
            ) => Ok(Command::Expr(self.expr_head()?)),
            // `| @(a; b)` — a list literal as the command's
            // expression; `| @%(…)` the record array.
            Some(Token::At)
                if matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                    || (matches!(self.toks.get(self.pos + 1), Some(Token::Percent))
                        && matches!(self.toks.get(self.pos + 2), Some(Token::LParen))) =>
            {
                Ok(Command::Expr(self.additive()?))
            }
            // `| @@.` — the node references in filing order.
            Some(Token::AtAt) => {
                self.pos += 1;
                self.expect_dot("@@")?;
                Ok(Command::Recall(RegRef::NodeWhole))
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
                // `| @*` — retired (draft 157): the pointer, not
                // the row reading followed by a stray star.
                if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "*")
                {
                    return Err(QuarbError::parse(
                        "'@*' is retired: the context is '@_' (the topics), '@@_' \
                         (the positions; '@@_::prop' maps over them), '#.' / '#.*' (the \
                         table of the named / of every value regula) or '##.' / '##.*' \
                         (of the node regulae)",
                    ));
                }
                // `| @` bare — the row reading (spec: Command
                // Libraries, "A library value"): a library value as
                // its rows, a record of lists as a list of records.
                if !matches!(self.peek(), Some(Token::Name { text, .. }) if text == ".") {
                    return Ok(Command::Func(FnCall {
                        name: "@".into(),
                        args: Vec::new(),
                    }));
                }
                self.expect_dot("@")?;
                Ok(Command::Recall(RegRef::Whole))
            }
            // `| #` bare — the table reading (spec: The Table Kind):
            // a list of records as a frame, a list of lists as an
            // array; `| #(…)` the literal as the value.
            Some(Token::Hash) => {
                if matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                    || self.table_count_ahead()
                {
                    return self.table_command();
                }
                self.pos += 1;
                Ok(Command::Func(FnCall {
                    name: "#".into(),
                    args: Vec::new(),
                }))
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
            // `| %.` — the named value view, as a record; `| %..`
            // — the full value view, anonymous regulae included
            // under their positions; `| %%(...)` — the named view
            // enriched with the given fields (args after registers,
            // args win on a shared name); `| %%.` — the named node
            // view; `| %%..` — the full node view.
            Some(Token::Percent) => {
                let head = self.pos;
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
                    return Ok(Command::Recall(match self.view_dots("%%")? {
                        1 => RegRef::NodeRecord,
                        _ => RegRef::NodeFullRecord,
                    }));
                }
                if matches!(self.peek(), Some(Token::LParen)) {
                    let call = self.record_args()?;
                    // `| %(v = $_) ++ %(w = 1)` — the constructor is
                    // the head of an expression that continues past
                    // its closing parenthesis (spec: Literals and
                    // constructors).
                    if self.expr_continues() {
                        self.pos = head;
                        return Ok(Command::Expr(self.additive()?));
                    }
                    validate_record(&call)?;
                    return Ok(Command::Func(call));
                }
                // `| %` bare — the column reading: a library value
                // as its columns, a list of records as a record of
                // lists.
                // `%,` — the session memory as a record (draft 166)
                if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == ",")
                {
                    self.pos += 1;
                    return Ok(Command::Recall(RegRef::SessionRecord));
                }
                if !self.at_view_dots() {
                    return Ok(Command::Func(FnCall {
                        name: "%".into(),
                        args: Vec::new(),
                    }));
                }
                Ok(Command::Recall(match self.view_dots("%")? {
                    1 => RegRef::Record,
                    _ => RegRef::FullRecord,
                }))
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
            // `| ,name` — the session push (spec: Capsa Operations,
            // "Session memory", draft 166)
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if text.starts_with(',') => {
                let name = text[1..].to_string();
                self.pos += 1;
                if name.is_empty()
                    || !name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                {
                    return Err(QuarbError::parse(
                        "the session push names its value: '| ,df' files the topic under 'df' for the session",
                    ));
                }
                if matches!(self.peek(), Some(Token::LParen)) {
                    return Err(QuarbError::parse(format!(
                        "the session push takes no body: '| ,{name}' files the topic; compute it first ('| expr | ,{name}')"
                    )));
                }
                Ok(Command::SessionPush(name))
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
                // Validate the pattern at parse time, under the
                // `i m s x` modifiers it will run with.
                let flags = super::lexer::subst_flags(mods);
                Regex::new(&format!("{flags}{pattern}"))
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
            // is a whole-context operation and lives on `@|`, except
            // on a table, where `| [n]` is the row and `| [a..b]` the
            // rows (spec: The Table Kind): the stdlib decides at the
            // run, by the topic's kind.
            // `| [[x]]`, `| [[a..b]]` — the lookup by label on the
            // topic (spec: Indexing a Value, draft 166), beside `| [n]`
            Some(Token::LBracket)
                if matches!(self.toks.get(self.pos + 1), Some(Token::LBracket)) =>
            {
                let labels = self.label_brackets()?;
                return Ok(Command::Expr(Operand::LabelIndex {
                    base: Box::new(Operand::Topic),
                    labels,
                }));
            }
            Some(Token::LBracket) => {
                let positional = || {
                    QuarbError::parse(
                        "positional selection is whole-context; write '@| [n]' / \
                         '@| [a..b]' (a plain '| [cond]' filters per capsa)",
                    )
                };
                let mut e = match self.predicate()? {
                    Predicate::Expr(e) => e,
                    Predicate::Index(n) => {
                        return Ok(Command::Func(FnCall {
                            name: "[]".into(),
                            args: vec![Arg::Lit(Value::Int(n))],
                        }));
                    }
                    Predicate::Range(a, b) => {
                        return Ok(Command::Func(FnCall {
                            name: "[]".into(),
                            args: vec![Arg::Range(a, b)],
                        }));
                    }
                    // `| [2; 4; 5]` — the listed rows or items, one
                    // argument per part; `| [$.pos]` the expression,
                    // read per capsa.
                    Predicate::Positions(parts) => {
                        return Ok(Command::Func(FnCall {
                            name: "[]".into(),
                            args: parts
                                .into_iter()
                                .map(|p| match p {
                                    Predicate::Index(n) => Arg::Lit(Value::Int(n)),
                                    Predicate::Range(a, b) => Arg::Range(a, b),
                                    _ => unreachable!("a position list holds positions and ranges"),
                                })
                                .collect(),
                        }));
                    }
                    Predicate::Computed(op) => {
                        return Ok(Command::Func(FnCall {
                            name: "[]".into(),
                            args: vec![Arg::Expr(op)],
                        }));
                    }
                };
                // Glued predicates chain as on a path step:
                // `| [a][b]` ≡ `| [a] | [b]` — every capsa passes
                // both, in order.
                while matches!(self.peek(), Some(Token::LBracket)) {
                    match self.predicate()? {
                        Predicate::Expr(next) => e = PredExpr::And(Box::new(e), Box::new(next)),
                        Predicate::Index(_)
                        | Predicate::Range(_, _)
                        | Predicate::Positions(_)
                        | Predicate::Computed(_) => return Err(positional()),
                    }
                }
                Ok(Command::Filter(e))
            }
            // A value-expression command starts with a projection, a
            // parenthesized group, or an interpolated string:
            // `| ::price * ::qty`, `| "${::name} (${::age})"`.
            // (A bare path start is a navigation command, handled
            // above; parenthesized paths remain expressions.)
            // (the full expression: a comparison and a bare conditional
            // follow the head, `| (::a = ::b) ? 1 : 0`, draft 166)
            Some(Token::ColonColon | Token::ColonColonColon | Token::LParen | Token::Interp(_)) => {
                Ok(Command::Expr(self.expr_head()?))
            }
            // A literal where a command name would stand opens an
            // expression (spec: Literals and constructors): `| 0`
            // yields 0 per thread, `| 10 + $_` is `| + 10`, `| "x"`
            // the text, `| 2013-01-01 + 1_d` the instant (a literal
            // of any kind, draft 166); a bare word stays a command call.
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if !matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                && self.literal_word(text) =>
            {
                Ok(Command::Expr(self.expr_head()?))
            }
            Some(Token::Name { quoted: true, .. }) => Ok(Command::Expr(self.expr_head()?)),
            // `| date(2013-01-01) + 1_d` — a command that takes no
            // operand of its own, written with a parenthesis holding
            // an operand, is the call operand it is anywhere else
            // (draft 166): the parenthesis's value piped into it, then
            // an expression as a literal opens
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                && crate::syntax::man::operand_bound(text) == Some(0)
                // (a reducer keeps its refusal of a key written as an
                // operand, with the pointer to `--of`: `sum(:x)`)
                && crate::syntax::registry::spec(text).is_some_and(|s| s.agg.is_none())
                && !matches!(self.toks.get(self.pos + 2), Some(Token::RParen | Token::DashDash))
                && !matches!(self.toks.get(self.pos + 2), Some(Token::Name { text: t, quoted: false, .. }) if lexer::is_option_word(t)) =>
            {
                Ok(Command::Expr(self.expr_head()?))
            }
            // `| q(cmd)` — the quarq literal stands where a literal
            // stands (spec: Quarqs); `q` is no command.
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if text == "q" && matches!(self.toks.get(self.pos + 1), Some(Token::LParen)) => {
                Ok(Command::Expr(self.additive()?))
            }
            _ => {
                let call = self.func_call()?;
                // `now()` refuses to stand after a pipe: it takes no topic.
                if call.name == "now" {
                    return Err(QuarbError::parse(
                        "now() is a call operand (the invocation instant); it takes no \
                         topic — use it in expression position: [::date > now() - 12h]",
                    ));
                }
                self.check_unit_call(&call)?;
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
                    | Token::ArrowIn { .. }
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
            // `| //x:%(…)` — the projection operator on the path
            Some(Token::ProjectColon) => true,
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                matches!(
                    text.as_str(),
                    "+" | "-" | "*" | "div" | "idiv" | "mod" | "++" | "+@+"
                )
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

    /// The dots of a register view after its sigil: one (`%.`, the
    /// named view) or two (`%..`, the full view), as the count.
    fn view_dots(&mut self, sigil: &str) -> Result<usize> {
        match self.peek() {
            Some(Token::Name { text, .. }) if text == "." => {
                self.pos += 1;
                Ok(1)
            }
            // `%.*` — the full view (draft 163; `%..` is retired,
            // the doubled dot being the penultimate).
            Some(Token::Name { text, .. }) if text == ".*" => {
                self.pos += 1;
                Ok(2)
            }
            Some(Token::Name { text, .. }) if text == ".." => Err(QuarbError::parse(format!(
                "'{sigil}..' is retired: the full view is '{sigil}.*' (the doubled dot is the penultimate regula, '$..')"
            ))),
            _ => Err(QuarbError::parse(format!("expected '.' after '{sigil}'"))),
        }
    }

    /// Whether the next token is a view's dot, star or dots.
    fn at_view_dots(&self) -> bool {
        matches!(self.peek(), Some(Token::Name { text, .. }) if text == "." || text == ".*" || text == "..")
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
        // The body is spliced text: `$$0` in it is judged where the
        // expansion lands, so the body itself admits it and records
        // that it does (ruling of 2026-10-20).
        let outer_body = std::mem::replace(&mut self.in_def_body, true);
        let outer_uses = std::mem::replace(&mut self.body_uses_driver, false);
        let outer_units = std::mem::take(&mut self.body_unit_operands);
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
        let uses_driver = self.body_uses_driver;
        let unit_operands = std::mem::replace(&mut self.body_unit_operands, outer_units);
        self.in_def_body = outer_body;
        self.body_uses_driver = outer_uses;
        self.expect(Token::Semi, "';' to end the definition")?;
        self.defs.entries.push((
            name,
            Def {
                params,
                rest: None,
                data_aware: false,
                uses_driver,
                unit_operands,
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
                uses_driver: false,
                unit_operands: Vec::new(),
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
        usage: RegUsage,
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
        let global = self.expander.usage_of_query(q);
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
                form => Operand::Lit(Value::Str(crate::syntax::unparse::operand_text(form))),
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

        let mut bindings: Vec<(String, MacroBinding)> = def
            .params
            .iter()
            .zip(&args)
            .map(|(p, a)| (p.clone(), MacroBinding::One(a.clone())))
            .collect();
        if let Some(rest) = &def.rest {
            bindings.push((rest.clone(), MacroBinding::Rest(args[n..].to_vec())));
        }
        let values = self
            .expander
            .expand(&name, &body, &bindings, def.data_aware)?;
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
    fn check_bang(&mut self, name: &str, def: &Def, bang: bool) -> Result<()> {
        // The splice site judges the body's `$$0`: a join condition
        // admits it; elsewhere the invocation is the parse error.
        // Inside another body, the verdict is deferred once more.
        if def.uses_driver && self.correlation_depth == 0 {
            if self.in_def_body {
                self.body_uses_driver = true;
            } else {
                return Err(QuarbError::parse(format!(
                    "in expansion of '&{name}': {DRIVER_PLACEMENT}"
                )));
            }
        }
        // The body's unit operands, likewise: judged here when a
        // dataset is at hand, deferred once more inside another body.
        if !def.unit_operands.is_empty() {
            match self.expander.unit_names() {
                Some(names) => {
                    for (command, operand) in &def.unit_operands {
                        if let Some(why) = unit_operand_refusal(command, operand, &names) {
                            return Err(QuarbError::parse(format!(
                                "in expansion of '&{name}': {why}"
                            )));
                        }
                    }
                }
                None if self.in_def_body => {
                    self.body_unit_operands
                        .extend(def.unit_operands.iter().cloned());
                }
                None => {}
            }
        }
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

    /// The operand of `convert(U)`, `unit(U)` and `magnitude(U)` must
    /// name a unit the query knows (spec: The conversion): the
    /// built-in table, a model's declarations, an adapter's minted
    /// units. With a dataset at hand the operand is judged here; a
    /// fragment body parsed without one (a definitions file) records
    /// it for the splice site.
    fn check_unit_call(&mut self, call: &FnCall) -> Result<()> {
        if !matches!(call.name.as_str(), "convert" | "unit" | "magnitude") {
            return Ok(());
        }
        let Some(Arg::Lit(Value::Str(operand))) = call.args.first() else {
            return Ok(());
        };
        if operand.is_empty() {
            return Ok(());
        }
        match self.expander.unit_names() {
            Some(names) => match unit_operand_refusal(&call.name, operand, &names) {
                Some(why) => Err(QuarbError::parse(why)),
                None => Ok(()),
            },
            None if self.in_def_body => {
                self.body_unit_operands
                    .push((call.name.clone(), operand.clone()));
                Ok(())
            }
            None => Ok(()),
        }
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
            // A macro expands to text, reparsed here as a query with
            // the splice site's scopes (a joined position admits its
            // `$$0`).
            DefBody::Macro(_) => {
                let invited = invites_of(self.expander, &args);
                let text = self.expand_macro_path_text(&name, &def, args)?;
                let q = self.parse_expansion_query(&name, &text)?;
                self.record_captures(&name, &invited, self.expander.usage_of_query(&q));
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
        // `| &$.p` — the quarq a register holds, spliced at the
        // recall (spec: Quarqs); the plain pipe only, as the body
        // runs per capsa
        if matches!(self.toks.get(self.pos + 1), Some(Token::Dollar)) {
            if pipe != "|" {
                return Err(QuarbError::parse(
                    "a held quarq runs per capsa: splice it with '| &$.p'",
                ));
            }
            self.pos += 1;
            pipeline.push(Command::Splice(self.splice_register()?));
            return Ok(());
        }
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
                let invited = invites_of(self.expander, &args);
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
                self.record_captures(&name, &invited, self.expander.usage_of_commands(&commands));
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
                    // `/x:%(…)` — the projection operator closes it
                    | Token::ProjectColon
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
            // `:name` — the core rung ends the branch too (draft
            // 157: the ladder counts from the core).
            if matches!(tok, Token::Field) {
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
                let invited = invites_of(self.expander, &args);
                let text = self.expand_macro_path_text(&name, &def, args)?;
                let q = self.parse_expansion_query(&name, &text)?;
                self.record_captures(&name, &invited, self.expander.usage_of_query(&q));
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
                        | Token::ArrowIn { .. }
                        | Token::DashDash
                        | Token::LParen
                        | Token::Amp
                        | Token::ColonColon
                        | Token::ColonColonColon
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
        p.correlation_depth = self.correlation_depth;
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
                Anchor::Id(op) => format!("((!{}))", crate::syntax::unparse::operand_text(&op)),
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
                        | Token::ArrowIn { .. }
                        | Token::DashDash
                        | Token::ColonColon
                        | Token::ColonColonColon
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
                } else if text == ".." {
                    // `$$..` / `((..))` — the penultimate mark.
                    Some((Anchor::MarkBack(2), 3))
                } else if let Some(n) = text
                    .strip_prefix(".-")
                    .or_else(|| text.strip_prefix('-'))
                    .and_then(|d| d.parse::<usize>().ok())
                    .filter(|&n| n >= 1)
                {
                    // `$$.-N` / `((.-N))` — the mark N back from the
                    // latest.
                    Some((Anchor::MarkBack(n), 3))
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
    /// The items of a list literal after its `(`, through the `)`;
    /// with them, whether every item was a bare name (a header's
    /// shape). With `names`, a bare word standing alone is a string
    /// (the header of a table literal: `@(name; age)`); elsewhere a
    /// bare word in a body names a command and is refused as such.
    fn list_items(&mut self, names: bool) -> Result<(Vec<Operand>, bool)> {
        let mut items = Vec::new();
        let mut bare = true;
        loop {
            if matches!(self.peek(), Some(Token::RParen)) {
                self.pos += 1;
                break;
            }
            // A constructor takes no option yet (spec: Literals and
            // constructors): a dash-led last field is refused by
            // name; a hop as an item is `@((--x))` or `@(--x;)`.
            if let Some(opt) = self.option_item() {
                return Err(QuarbError::parse(format!(
                    "'@(…)' takes no option ('{opt}' given); its items are values — a hop as \
                     an item is written '@((--x))' or '@(--x;)'"
                )));
            }
            let word = match (self.peek(), self.toks.get(self.pos + 1)) {
                (
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }),
                    Some(Token::Comma | Token::Semi | Token::RParen),
                ) if text.parse::<f64>().is_err() => Some(text.clone()),
                _ => None,
            };
            let item = match word {
                Some(w) if names => {
                    self.pos += 1;
                    Operand::Lit(Value::Str(w))
                }
                _ => {
                    bare = false;
                    self.additive()?
                }
            };
            items.push(item);
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
        let bare = bare && !items.is_empty();
        Ok((items, bare))
    }

    /// `#(…)` after its sigil (spec: The Table, Table Construction,
    /// draft 166): a frame is an optional header, a `@(…)` list of
    /// names standing first (bare or quoted; a number names its
    /// column by its text), then rows, each a `%(…)` record; a
    /// MATRIX is rows that are all lists, `#(@(1; 2); @(3; 4))`, a
    /// headerless table. A list after a record row, or a record
    /// after a list row, is refused: the rows are of one sigil. A
    /// single list alone is a one-row matrix unless its items are
    /// all names, which points at `--header`. Positional records
    /// without a header are the array face, the positions naming
    /// the columns; named rows without a header infer it.
    fn table_literal(&mut self) -> Result<Operand> {
        self.expect(Token::LParen, "'(' after '#'")?;
        let mut header: Option<Vec<String>> = None;
        let mut rows: Vec<TableRow> = Vec::new();
        // the first list, held until the next item says whether it
        // is the header (records follow) or a matrix row
        let mut held: Option<Vec<Operand>> = None;
        let mut first = true;
        let header_of = |items: Vec<Operand>| -> Result<Vec<String>> {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Operand::Lit(Value::Str(s)) => names.push(s),
                    Operand::Lit(v @ (Value::Int(_) | Value::Float(_))) => {
                        names.push(v.to_string())
                    }
                    _ => {
                        return Err(QuarbError::parse(
                            "a table's header is a list of names, bare or quoted (#(@(name; age); …)); a value computed there names nothing",
                        ));
                    }
                }
            }
            Ok(names)
        };
        loop {
            if matches!(self.peek(), Some(Token::RParen)) {
                self.pos += 1;
                break;
            }
            match (self.peek(), self.toks.get(self.pos + 1)) {
                (Some(Token::At), Some(Token::LParen)) => {
                    if !first && held.is_none() && !matches!(rows.last(), Some(TableRow::List(_))) {
                        return Err(QuarbError::parse(
                            "a list after a record row: a table's rows are all records (a frame, its header the list standing first) or all lists (a matrix, #(@(1; 2); @(3; 4)))",
                        ));
                    }
                    self.pos += 2;
                    let (items, _) = self.list_items(true)?;
                    if first {
                        held = Some(items);
                    } else {
                        // a second list: the held one was a matrix row
                        if let Some(h) = held.take() {
                            rows.push(TableRow::List(h));
                        }
                        rows.push(TableRow::List(items));
                    }
                }
                (Some(Token::Percent), Some(Token::LParen)) => {
                    if matches!(rows.last(), Some(TableRow::List(_))) {
                        return Err(QuarbError::parse(
                            "a record after a matrix row: a table's rows are all lists (a matrix) or all records (a frame, its header the list standing first)",
                        ));
                    }
                    if let Some(h) = held.take() {
                        header = Some(header_of(h)?);
                    }
                    self.pos += 2;
                    rows.push(TableRow::Record(self.record_row()?));
                }
                _ if self.option_item().is_some() => {
                    let opt = self.option_item().expect("checked");
                    return Err(QuarbError::parse(format!(
                        "'#(…)' takes its option alone, and '{opt}' stands beside an item: the \
                         rows of a reshape come through the pipe ('| #(--width=n)'); a \
                         literal's rows are written in it, without an option"
                    )));
                }
                _ => {
                    return Err(QuarbError::parse(
                        "a table holds record rows %(…) after an optional header list of names, #(@(name; age); %(\"Braund\"; 22)), or list rows for a matrix, #(@(1; 2); @(3; 4))",
                    ));
                }
            }
            first = false;
            match self.peek() {
                Some(Token::Comma | Token::Semi) => self.pos += 1,
                Some(Token::RParen) => {}
                _ => {
                    return Err(QuarbError::parse(
                        "a table separates its rows with ';': #(@(1; 2); @(3; 4))",
                    ));
                }
            }
        }
        // a single list alone: a one-row matrix, unless its items are
        // all names, which is a header of no rows (draft 166)
        if let Some(h) = held.take() {
            if !h.is_empty() && h.iter().all(|i| matches!(i, Operand::Lit(Value::Str(_)))) {
                let names: Vec<String> = h
                    .iter()
                    .map(|i| match i {
                        Operand::Lit(Value::Str(s)) => s.clone(),
                        _ => String::new(),
                    })
                    .collect();
                return Err(QuarbError::parse(format!(
                    "'#(@({0}))' alone is retired: a header of no rows; write the rows after it, or build the table from data with '#(--header=@({1}))'",
                    names.join("; "),
                    names
                        .iter()
                        .map(|n| format!(":{n}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )));
            }
            rows.push(TableRow::List(h));
        }
        Ok(Operand::Table { header, rows })
    }

    /// Whether `#` is followed by a parenthesized integer, which the
    /// lexer hands over as the rounded register `(n)`: `$` and the
    /// digits. The spelling `#(n)` is retired (draft 164).
    fn table_count_ahead(&self) -> bool {
        matches!(self.toks.get(self.pos + 1), Some(Token::Dollar))
            && matches!(self.toks.get(self.pos + 2), Some(Token::Name { text, quoted: false, .. }) if text.parse::<i64>().is_ok())
    }

    /// The pointer for the retired `#(n)`: a number is no row; the
    /// width is the option.
    fn retired_table_count(&self) -> Option<QuarbError> {
        if self.table_count_ahead()
            && let Some(Token::Name { text, .. }) = self.toks.get(self.pos + 2)
        {
            return Some(QuarbError::parse(format!(
                "'#({text})' is retired: a number is no row; write '#(--width={text})' \
                 ('-w={text}') to lay the list in rows of {text}"
            )));
        }
        None
    }

    /// Whether `#(` opens on an option (`--width=n`, `-w=n`,
    /// `--header=list`, `-h=list`): the reshape, not the literal.
    fn table_option_ahead(&self) -> bool {
        matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
            && match self.toks.get(self.pos + 2) {
                Some(Token::DashDash) => true,
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) => lexer::is_option_word(text),
                _ => false,
            }
    }

    /// The option a constructor's item slot holds, if any (spec:
    /// Call Operands, draft 164 — one rule for every parenthesis): a
    /// dash-led field opens the option segment, in `@(…)` / `%(…)` /
    /// `#(…)` as in a command's parenthesis, unless the trailing
    /// separator keeps its operand reading (`@(--author;)`, the
    /// hop's targets; `@((--author))` likewise, a group being no
    /// dash-led field). The spelling, for the refusal.
    fn option_item(&self) -> Option<String> {
        if !self.option_ahead() || self.trailing_separator_ahead() {
            return None;
        }
        Some(match (self.peek(), self.toks.get(self.pos + 1)) {
            (
                Some(Token::DashDash),
                Some(Token::Name {
                    text,
                    quoted: false,
                    glued: true,
                }),
            ) => format!("--{text}"),
            (Some(Token::DashDash), _) => "--".to_string(),
            (Some(Token::Name { text, .. }), _) => text.clone(),
            _ => "-".to_string(),
        })
    }

    /// Whether the field at the cursor is followed by the trailing
    /// separator: a `;` (or `,`) at this depth whose next token is
    /// the closing `)`. That empty last field closes the operands,
    /// so no option segment opens and a dash-led operand keeps its
    /// reading (`f(--author;)`, `@(--author;)`); a `;` followed by
    /// an operand after a dash-led field stays refused.
    fn trailing_separator_ahead(&self) -> bool {
        let mut depth = 0usize;
        let mut i = self.pos;
        while let Some(t) = self.toks.get(i) {
            match t {
                Token::LParen | Token::LBracket | Token::MarkOpen | Token::IdAnchorOpen => {
                    depth += 1
                }
                Token::RParen | Token::RBracket => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                Token::Semi | Token::Comma if depth == 0 => {
                    return matches!(self.toks.get(i + 1), Some(Token::RParen));
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// `#(--width=n)` / `#(-w=n)` / `#(--header=list)` / `#(-h=list)`
    /// (spec: Literals and constructors): the reshape's option,
    /// standing alone in the parenthesis, as the `#` command's
    /// argument — the column count, or the header's names (their
    /// count the width). An item beside the option is refused, as
    /// is an option the constructor does not take.
    fn table_options(&mut self) -> Result<FnCall> {
        self.pos += 2; // `#` `(`
        let mut opts: Vec<(String, Option<Operand>)> = Vec::new();
        loop {
            // the options stand apart by `;` (`#(-w=2; -i=:1)`)
            if !opts.is_empty() && matches!(self.peek(), Some(Token::Semi | Token::Comma)) {
                self.pos += 1;
            }
            let name = match (self.peek(), self.toks.get(self.pos + 1)) {
                (Some(Token::RParen), _) => {
                    self.pos += 1;
                    break;
                }
                (
                    Some(Token::DashDash),
                    Some(Token::Name {
                        text,
                        quoted: false,
                        glued: true,
                    }),
                ) => {
                    let t = format!("--{text}");
                    self.pos += 2;
                    t
                }
                (
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }),
                    _,
                ) if text.starts_with('-') => {
                    let t = text.clone();
                    self.pos += 1;
                    t
                }
                _ => {
                    let opt = opts
                        .last()
                        .map_or("the option".to_string(), |(n, _)| format!("'{n}'"));
                    return Err(QuarbError::parse(format!(
                        "'#(…)' takes its option alone, and an item stands beside {opt}: the \
                         rows of a reshape come through the pipe ('| #(--width=n)'); a \
                         literal's rows are written in it, without an option"
                    )));
                }
            };
            let value = if matches!(self.peek(), Some(Token::OptEq)) {
                self.pos += 1;
                // The header's names read bare or quoted, as a
                // literal's header does; so does the header's name.
                if matches!(name.as_str(), "--name" | "-n")
                    && let Some(Token::Name { text, .. }) = self.peek()
                {
                    let text = text.clone();
                    self.pos += 1;
                    Some(Operand::Lit(Value::Str(text)))
                } else if matches!(self.peek(), Some(Token::At))
                    && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                {
                    self.pos += 2;
                    let (items, _) = self.list_items(true)?;
                    Some(Operand::List(items))
                } else if matches!(self.peek(), Some(Token::Percent))
                    && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                {
                    // the typed header, a record of names and types
                    self.pos += 2;
                    Some(Operand::Row(self.record_row()?))
                } else if matches!(self.peek(), Some(Token::LParen))
                    && matches!(name.as_str(), "--index" | "-i")
                {
                    // `-i=(:first; :second)` — the composite index, the
                    // levels outer first (spec: The Table, "The index")
                    Some(self.projector_list()?)
                } else {
                    Some(self.unary()?)
                }
            } else {
                None
            };
            opts.push((name, value));
        }
        enum Header {
            Names(Vec<Value>),
            Computed(Operand),
        }
        let mut width: Option<i64> = None;
        let mut header: Option<Header> = None;
        let mut index: Option<Operand> = None;
        let mut header_name: Option<String> = None;
        for (name, value) in opts {
            match name.as_str() {
                // `--name=word` — the header's name (draft 166), beside
                // `--header` or alone on an existing table
                "--name" | "-n" => {
                    header_name = Some(match value {
                        Some(Operand::Lit(Value::Str(s))) if !s.is_empty() => s,
                        Some(Operand::Lit(v @ (Value::Int(_) | Value::Float(_)))) => v.to_string(),
                        _ => {
                            return Err(QuarbError::parse(format!(
                                "'#({name}=word)' names the header, a word bare or quoted"
                            )));
                        }
                    });
                }
                // `--index=:col` / `-i=:col<int>`: the index named, a
                // typed projector declaring it (spec: The Table)
                "--index" | "-i" => {
                    let Some(mut op) = value else {
                        return Err(QuarbError::parse(format!(
                            "'#({name}=:col)' names the index column"
                        )));
                    };
                    head_projections(&mut op);
                    match &op {
                        Operand::Lit(Value::Projection { .. }) | Operand::Lit(Value::List(_)) => {
                            index = Some(op)
                        }
                        Operand::Lit(Value::Str(s)) => {
                            index = Some(Operand::Lit(Value::projection(1, s.clone())))
                        }
                        Operand::Lit(v @ (Value::Int(_) | Value::Float(_))) => {
                            index = Some(Operand::Lit(Value::projection(1, v.to_string())))
                        }
                        _ => index = Some(op),
                    }
                }
                "--width" | "-w" => {
                    width = Some(match value {
                        Some(Operand::Lit(Value::Int(n))) => n,
                        _ => {
                            return Err(QuarbError::parse(format!(
                                "'#({name}=n)' takes the column count, a number"
                            )));
                        }
                    });
                }
                "--header" | "-h" => {
                    // The record form of the header, `%(zip = "text")`,
                    // is retired (draft 166): the header is a list of
                    // typed projectors.
                    if let Some(Operand::Row(fields)) = &value {
                        let spelled: Vec<String> = fields
                            .iter()
                            .map(|(k, op)| {
                                let word = match op {
                                    Operand::Lit(Value::Str(w)) => w.clone(),
                                    _ => "type".to_string(),
                                };
                                format!(":{}<{word}>", k.clone().unwrap_or_else(|| "name".into()))
                            })
                            .collect();
                        return Err(QuarbError::parse(format!(
                            "'#({name}=%(…))' is retired: the header is a list of typed projectors, \
                             '#({name}=@({}))'",
                            spelled.join("; ")
                        )));
                    }
                    let items = match value {
                        Some(Operand::List(items)) => items,
                        Some(Operand::Lit(Value::List(l))) => {
                            l.iter().cloned().map(Operand::Lit).collect()
                        }
                        // A header computed elsewhere (`(t | header)`,
                        // a capsa): read when the table is built.
                        Some(
                            op @ (Operand::Group(_)
                            | Operand::Recall(_)
                            | Operand::Field { .. }
                            | Operand::Rel { .. }
                            | Operand::Param(_)
                            | Operand::Piped { .. }
                            | Operand::Index { .. }
                            | Operand::PeerReg { .. }
                            | Operand::Topic),
                        ) => {
                            header = Some(Header::Computed(op));
                            continue;
                        }
                        _ => {
                            return Err(QuarbError::parse(format!(
                                "'#({name}=…)' takes the column names as a list, \
                                 '#({name}=@(a; b; c))', or the typed header as a record, \
                                 '#({name}=%(a = \"int\"; b = \"text\"))'"
                            )));
                        }
                    };
                    // the items: names, bare or quoted, or projectors,
                    // typed or not (`:zip<text>`; a bare `:zip` names
                    // its column undeclared, as the name does)
                    let mut names = Vec::with_capacity(items.len());
                    for mut item in items {
                        head_projections(&mut item);
                        match item {
                            Operand::Lit(Value::Str(s)) => names.push(Value::Str(s)),
                            Operand::Lit(v @ (Value::Int(_) | Value::Float(_))) => {
                                names.push(Value::Str(v.to_string()))
                            }
                            Operand::Lit(p @ Value::Projection { .. }) => names.push(p),
                            _ => {
                                return Err(QuarbError::parse(
                                    "a header is a list of names, bare or quoted, or of projectors \
                                     ('#(--header=@(name; age))', '#(--header=@(:zip<text>; :amount<float>))'); \
                                     a value computed there names nothing",
                                ));
                            }
                        }
                    }
                    header = Some(Header::Names(names));
                }
                other => {
                    return Err(QuarbError::parse(format!(
                        "'#(…)' takes '--width=n' ('-w=n'), '--header=list' ('-h=list'), \
                         '--index=:col' ('-i=:col') or '--name=word' ('-n=word'); '{other}' given"
                    )));
                }
            }
        }
        let arg = match (width, header) {
            (Some(_), Some(_)) => {
                return Err(QuarbError::parse(
                    "'#(…)' takes one of '--width=n', which lays the list in rows of n, and \
                     '--header=list', which names the columns (their count the width); both given",
                ));
            }
            (Some(n), None) => Some(Arg::Lit(Value::Int(n))),
            (None, Some(Header::Names(h))) => Some(Arg::Lit(Value::list(h))),
            (None, Some(Header::Computed(op))) => Some(Arg::Expr(op)),
            (None, None) if index.is_some() || header_name.is_some() => None,
            (None, None) => {
                return Err(QuarbError::parse(
                    "'#(…)' names no option: '#(--width=n)' ('-w=n') lays the list in rows \
                     of n, '#(--header=list)' ('-h=list') names its columns, '#(--index=:col)' \
                     ('-i=:col') names the index, '#(--name=word)' names the header",
                ));
            }
        };
        let mut args: Vec<Arg> = arg.into_iter().collect();
        if let Some(op) = index {
            args.push(Arg::Opt {
                name: "--index".into(),
                value: Some(op),
            });
        }
        if let Some(n) = header_name {
            args.push(Arg::Opt {
                name: "--name".into(),
                value: Some(Operand::Lit(Value::Str(n))),
            });
        }
        Ok(FnCall {
            name: "#".into(),
            args,
        })
    }

    /// Whether a literal or a constructor stands where a command
    /// name would (spec: Literals and constructors): a number, a
    /// quoted string, `null`, `true`, `false`, a quantity literal,
    /// `@(…)`, `@%(…)`, or `#(…)` as a literal — not the bare sigils
    /// (the readings), not `#(--width=n)` (the reshape), and not
    /// `%(…)`, which keeps its own arm. A parenthesized group is
    /// admitted after `@|` as the expression's own delimiter.
    fn expr_head_ahead(&self) -> bool {
        match self.peek() {
            Some(Token::LParen) => true,
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                (!matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                    && self.literal_word(text))
                    || (text == "q" && matches!(self.toks.get(self.pos + 1), Some(Token::LParen)))
            }
            Some(Token::Name { quoted: true, .. } | Token::Interp(_)) => true,
            Some(Token::At) => {
                matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                    || (matches!(self.toks.get(self.pos + 1), Some(Token::Percent))
                        && matches!(self.toks.get(self.pos + 2), Some(Token::LParen)))
            }
            Some(Token::Hash) => {
                matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                    && !self.table_option_ahead()
            }
            _ => false,
        }
    }

    /// Whether the token at the head can only open a value, so the
    /// `=` sigil may be dropped (spec: The Expression Head, draft
    /// 166): a literal (a number, a string, a date, a quantity,
    /// nan/inf, true/false/null), a constructor `@(` `%(` `#(`, a
    /// quarq `q(`, a session read `$,`. A parenthesis, a minus, a
    /// name and a fragment call keep the sigil.
    /// `.v(body){N}` — a quantifier after a push repeats it; after
    /// any other command it is refused.
    fn quantified_push(&mut self, command: Command) -> Result<Command> {
        let Some(&Token::Quant { min, max }) = self.peek() else {
            return Ok(command);
        };
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
        Ok(Command::Repeat {
            command: Box::new(command),
            min,
            max,
        })
    }

    /// After an explicit `=`: whether a command, not an expression,
    /// follows — a register push (`.f(0)`, `.`, `.%`) or the bare name
    /// of a library command (draft 171: the head's expression may be
    /// left out; the command is then the pipeline's first, on the root
    /// thread with no topic). A name followed by a parenthesis stays
    /// the call form (`= upper("ab")`: the first argument the input).
    fn command_head_ahead(&self) -> bool {
        let next_paren = matches!(self.toks.get(self.pos + 1), Some(Token::LParen));
        match self.peek() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                !self.literal_word(text)
                    && (text.starts_with('.')
                        || (!next_paren
                            && !text.starts_with('-')
                            && crate::syntax::registry::spec(text).is_some()))
            }
            _ => false,
        }
    }

    fn value_only_head(&self) -> bool {
        let next_paren = matches!(self.toks.get(self.pos + 1), Some(Token::LParen));
        match self.peek() {
            Some(Token::Name { quoted: true, .. }) | Some(Token::Interp(_)) => true,
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => {
                (text == "q" && next_paren)
                    || (!next_paren && !text.starts_with('-') && self.literal_word(text))
            }
            // `@(`, `%(`, `#(`, and the record-array sugar `@%(`
            Some(Token::At) => {
                next_paren
                    || (matches!(self.toks.get(self.pos + 1), Some(Token::Percent))
                        && matches!(self.toks.get(self.pos + 2), Some(Token::LParen)))
            }
            Some(Token::Percent | Token::Hash) => next_paren,
            Some(Token::Dollar) => {
                matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, .. }) if text.starts_with(','))
            }
            _ => false,
        }
    }

    /// Whether a bare word reads as a literal rather than text: a
    /// number, `null`, `true`, `false`, a quantity or a span.
    fn literal_word(&self, text: &str) -> bool {
        !matches!(
            literal_value(text, false, &|e| self.expander.unit_scale(e)),
            Value::Str(_)
        )
    }

    /// `| #(…)` after a pipe (spec: Literals and constructors): the
    /// option form `#(--width=n)` / `#(--header=list)` is the reshape
    /// of the list topic, a command; a literal with rows is its
    /// value, the head of an expression. `#(n)` and a header alone,
    /// `#(@(a; b; c))`, are retired with the pointer to the option.
    fn table_command(&mut self) -> Result<Command> {
        if let Some(err) = self.retired_table_count() {
            return Err(err);
        }
        if !self.table_option_ahead() {
            let expr = self.additive()?;
            if let Operand::Table {
                header: Some(h),
                rows,
            } = &expr
                && rows.is_empty()
            {
                let names = h.join("; ");
                return Err(QuarbError::parse(format!(
                    "'#(@({names}))' alone is retired: a header written as an item has its \
                     rows beside it; write '#(--header=@({names}))' ('-h=…') to name the \
                     columns of the list topic"
                )));
            }
            return Ok(Command::Expr(expr));
        }
        let call = self.table_options()?;
        // `| #(-w=3) x v` — the reshaped topic as the left operand
        // of the expression that follows.
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && arith_word(text).is_some()
        {
            let left = Operand::Piped {
                expr: Box::new(Operand::Topic),
                commands: vec![Command::Func(call)],
            };
            let left = self.multiplicative_from(left)?;
            return Ok(Command::Expr(self.additive_from(left)?));
        }
        Ok(Command::Func(call))
    }

    /// A table row's items after its `%(`, through the `)`: each
    /// positional or `name = value`, `;` between them.
    fn record_row(&mut self) -> Result<Vec<(Option<String>, Operand)>> {
        let mut items: Vec<(Option<String>, Operand)> = Vec::new();
        loop {
            if matches!(self.peek(), Some(Token::RParen)) {
                self.pos += 1;
                break;
            }
            let name = match (self.peek(), self.toks.get(self.pos + 1)) {
                (Some(Token::Name { text, quoted, .. }), Some(Token::Eq)) => {
                    // a quoted key is marked, so it stays text where a
                    // bare one reads as the literal it spells
                    let key = if *quoted {
                        format!("\u{0}{text}")
                    } else {
                        text.clone()
                    };
                    self.pos += 2;
                    Some(key)
                }
                _ => None,
            };
            // `price<float> = 5` — the declared field: the value
            // typed by the key's trait
            let typed = if name.is_none() {
                self.typed_key()?
            } else {
                None
            };
            let mut item = self.additive()?;
            if let (Some(key), Operand::Typed { inner, typing }) = (&name, &item) {
                return Err(typed_value_refusal(key, inner, typing.clone()));
            }
            let name = match typed {
                Some((key, typing)) => {
                    item = Operand::Typed {
                        inner: Box::new(item),
                        typing,
                    };
                    Some(key)
                }
                None => name,
            };
            // `#($.h...)` — the spread: the projectors a held list
            // holds, each its own column (spec: Projectors, draft 166)
            if name.is_none()
                && let Some(Token::Name {
                    text,
                    quoted: false,
                    glued: true,
                }) = self.peek()
                && text == "..."
            {
                self.pos += 1;
                item = Operand::Spread(Box::new(item));
            }
            items.push((name, item));
            match self.peek() {
                Some(Token::Comma | Token::Semi) => self.pos += 1,
                Some(Token::RParen) => {}
                _ => {
                    return Err(QuarbError::parse(
                        "a table row separates its items with ';': %(\"Braund\"; 22)",
                    ));
                }
            }
        }
        Ok(items)
    }

    /// `#(:A; :B)` / `#(:A; :B; -i=:A)` after the glued colon (spec:
    /// Projectors; The Table, "The index"): the columns of the table
    /// shape as a record row reads them, and the index option naming
    /// one of them, a projector typed or not.
    fn shape_columns(&mut self) -> Result<ProjShape> {
        let mut items: Vec<(Option<String>, Operand)> = Vec::new();
        let mut index: Option<Box<Operand>> = None;
        loop {
            if matches!(self.peek(), Some(Token::RParen)) {
                self.pos += 1;
                break;
            }
            if let Some(opt) = self.option_item() {
                if !matches!(opt.as_str(), "--index" | "-i") {
                    return Err(QuarbError::parse(format!(
                        "the table shape takes one option, '--index=:col' ('-i=:col'); '{opt}' given"
                    )));
                }
                self.pos += if opt.starts_with("--") { 2 } else { 1 };
                if !matches!(self.peek(), Some(Token::OptEq)) {
                    return Err(QuarbError::parse(
                        "'--index' names the index column: '#(:A; :B; -i=:A)'",
                    ));
                }
                self.pos += 1;
                let mut op = if matches!(self.peek(), Some(Token::LParen)) {
                    self.projector_list()?
                } else {
                    self.unary()?
                };
                head_projections(&mut op);
                index = Some(Box::new(op));
            } else {
                let name = match (self.peek(), self.toks.get(self.pos + 1)) {
                    (Some(Token::Name { text, .. }), Some(Token::Eq)) => {
                        let key = text.clone();
                        self.pos += 2;
                        Some(key)
                    }
                    _ => None,
                };
                let mut item = self.additive()?;
                if name.is_none()
                    && let Some(Token::Name {
                        text,
                        quoted: false,
                        glued: true,
                    }) = self.peek()
                    && text == "..."
                {
                    self.pos += 1;
                    item = Operand::Spread(Box::new(item));
                }
                items.push((name, item));
            }
            match self.peek() {
                Some(Token::Comma | Token::Semi) => self.pos += 1,
                Some(Token::RParen) => {}
                _ => {
                    return Err(QuarbError::parse(
                        "the table shape separates its items with ';': t:#(:A; :B)",
                    ));
                }
            }
        }
        Ok(ProjShape::Columns { items, index })
    }

    /// `@%(…)` after its `@%`: the table literal's shape read as a
    /// record array — the header required, every row a record.
    fn record_array(&mut self) -> Result<Operand> {
        let Operand::Table { header, rows } = self.table_literal()? else {
            unreachable!("the table literal parses to a table operand")
        };
        let Some(header) = header else {
            return Err(QuarbError::parse(
                "the record array opens with its header list: @%(@(name; age); %(\"Austen\"; 41))",
            ));
        };
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TableRow::Record(items) => items,
                TableRow::List(items) => items.into_iter().map(|e| (None, e)).collect(),
            })
            .collect();
        Ok(Operand::RecordArray { header, rows })
    }

    /// One index inside a value's brackets (spec: Indexing a Value,
    /// draft 166): a position, a range (`a..b`, `..n`, `n..`), a
    /// quoted column name, an expression of literals and registers
    /// (`[@(2; 4)]`, `[$.pos]`), or else a CONDITION, as the brackets
    /// on a path step read it: `t[:A > 0]`, `v[$_ > 2]`, `t[nulls = 0]`.
    fn index_spec(&mut self) -> Result<IndexSpec> {
        let start = self.pos;
        let ends =
            |p: &Self| matches!(p.peek(), Some(Token::RBracket | Token::Semi | Token::Comma));
        // `[@(2; 4)]`, `[$.pos]` — an index read from an expression
        // of literals and registers, when it stands alone
        if matches!(self.peek(), Some(Token::At | Token::Dollar))
            && let Ok(op) = self.unary()
            && node_free(&op)
            && ends(self)
        {
            return Ok(IndexSpec::Expr(Box::new(op)));
        }
        self.pos = start;
        let (text, quoted) = match self.peek() {
            Some(Token::Name { text, quoted, .. }) => (text.clone(), *quoted),
            _ => return self.index_condition(start),
        };
        self.pos += 1;
        if quoted && ends(self) {
            return Ok(IndexSpec::Name(text));
        }
        if quoted {
            return self.index_condition(start);
        }
        let closes = matches!(
            self.peek(),
            Some(Token::RBracket | Token::Semi | Token::Comma)
        );
        if closes {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(IndexSpec::At(n));
            }
            if let Some((a, b)) = text.split_once("..") {
                let end = |s: &str| -> Result<Option<i64>> {
                    if s.is_empty() {
                        Ok(None)
                    } else {
                        s.parse::<i64>().map(Some).map_err(|_| {
                            QuarbError::parse(format!("'{text}' is not a range of positions"))
                        })
                    }
                };
                return Ok(IndexSpec::Range(end(a)?, end(b)?));
            }
        }
        self.index_condition(start)
    }

    /// The condition in a value's brackets, read as a step's is
    /// (draft 166): `[:A > 0]`, `[$_ > 2]`, `[nulls = 0]`.
    fn index_condition(&mut self, start: usize) -> Result<IndexSpec> {
        self.pos = start;
        let expr = self.pred_or().map_err(|e| {
            QuarbError::parse(format!(
                "brackets on a value hold an index — a position ([2]), a range ([2..4], [..3]), a column name ([\"age\"]) — or a condition on its rows or items ([:A > 0], [$_ > 2]): {e}"
            ))
        })?;
        if !matches!(
            self.peek(),
            Some(Token::RBracket | Token::Semi | Token::Comma)
        ) {
            return Err(QuarbError::parse(
                "brackets on a value close after one index or condition: ']'",
            ));
        }
        Ok(IndexSpec::Cond(Box::new(expr)))
    }

    fn record_args(&mut self) -> Result<FnCall> {
        self.expect(Token::LParen, "'(' after '%'")?;
        let mut args = Vec::new();
        if !matches!(self.peek(), Some(Token::RParen)) {
            loop {
                self.record_item(&mut args, true)?;
                if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                    self.pos += 1;
                    // the trailing separator: the empty last field
                    if matches!(self.peek(), Some(Token::RParen)) {
                        break;
                    }
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
        // `@| @` and `@| %` bare — the row and column readings of
        // the stream (spec: Command Libraries, "A library value").
        // `@| #(--width=n)` / `@| #(--header=list)` — the reshape of
        // the stream (a literal with rows heads an expression, taken
        // before this).
        if matches!(self.peek(), Some(Token::Hash))
            && (matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                || self.table_count_ahead())
        {
            return match self.table_command()? {
                Command::Func(call) => Ok(call),
                _ => Err(QuarbError::parse(
                    "after '@|' the table sigil takes an option: '@| #(--width=n)' lays the stream in rows of n, '@| #(--header=list)' names its columns",
                )),
            };
        }
        if let Some(sigil @ (Token::At | Token::Percent | Token::Hash)) = self.peek()
            && !matches!(
                self.toks.get(self.pos + 1),
                Some(Token::LParen | Token::Percent)
            )
            && !matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, .. }) if text == ".")
        {
            let name = match sigil {
                Token::At => "@",
                Token::Percent => "%",
                _ => "#",
            };
            self.pos += 1;
            return Ok(FnCall {
                name: name.to_string(),
                args: Vec::new(),
            });
        }
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
        self.call_with_name(name)
    }

    /// The rest of a call once its name is read: the optional
    /// parentheses (operands, then the option segment) and the
    /// shape checks.
    fn call_with_name(&mut self, name: String) -> Result<FnCall> {
        let environment = crate::syntax::registry::split_env(&name).is_some();
        if !environment && let Some(pointer) = retired_command(&name) {
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
            let saved_keyed = self.keyed_args;
            self.keyed_args = crate::syntax::registry::known_keyed(&name);
            let parsed = self.func_args(&name, convention, &mut args);
            self.keyed_args = saved_keyed;
            self.arg_depth = saved_depth;
            parsed?;
            self.expect(Token::RParen, "')' to close the command's operands")?;
        }
        self.finish_call(name, args)
    }

    /// The argument list of a call, up to (not including) its
    /// closing parenthesis.
    /// A bare compound unit as a command operand: an unquoted word
    /// joined to further words by `/` and `^` up to the `;`, `,` or
    /// `)`, taken as the unit's text (`km/h`, `m/s^2`, `V/A`,
    /// `kg*m/s^2` — the `*` rides inside the word). A single word,
    /// or anything else, is left to the ordinary operand reader.
    fn unit_operand(&mut self) -> Option<String> {
        let start = self.pos;
        let mut text = match self.peek() {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if !text.starts_with('-') => text.clone(),
            _ => return None,
        };
        if !matches!(
            self.toks.get(self.pos + 1),
            Some(Token::Slash | Token::Caret)
        ) {
            return None;
        }
        self.pos += 1;
        loop {
            match self.peek() {
                Some(Token::Slash) => text.push('/'),
                Some(Token::Caret) => text.push('^'),
                Some(Token::Comma | Token::Semi | Token::RParen) => return Some(text),
                _ => {
                    self.pos = start;
                    return None;
                }
            }
            self.pos += 1;
            match self.peek() {
                Some(Token::Name {
                    text: word,
                    quoted: false,
                    ..
                }) => text.push_str(word),
                _ => {
                    self.pos = start;
                    return None;
                }
            }
            self.pos += 1;
        }
    }

    fn func_args(&mut self, name: &str, convention: bool, args: &mut Vec<Arg>) -> Result<()> {
        {
            if !matches!(self.peek(), Some(Token::RParen)) {
                loop {
                    // The option segment: the last `;`-field, opened
                    // by a dash-led word, `--` or the void `-=`; it
                    // runs to the closing parenthesis. The trailing
                    // separator closes the operands with an empty
                    // field instead, and a dash-led operand keeps its
                    // reading: `f(--author;)` (draft 164).
                    if self.option_ahead() && !self.trailing_separator_ahead() {
                        self.option_segment(name, args)?;
                        break;
                    }
                    if convention {
                        self.record_item(args, name != "group")?;
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
                    } else if name == "tree"
                        && matches!(self.peek(), Some(Token::Percent))
                        && matches!(
                            self.toks.get(self.pos + 1),
                            Some(Token::Comma | Token::Semi | Token::RParen)
                        )
                    {
                        // `tree(%)` is retired (draft 164, the one
                        // option rule): the properties record is the
                        // switch.
                        return Err(QuarbError::parse(
                            "'%' is retired as tree's operand: write --record or -r for the properties record",
                        ));
                    } else if name == "pivot"
                        && args.is_empty()
                        && matches!(self.peek(), Some(Token::LParen))
                    {
                        // `pivot((:A; :B); C; D)` — a projector list as
                        // the row key (spec: Tables and Reshapes, draft
                        // 166): the result indexed by it
                        match self.projector_list()? {
                            Operand::Lit(v) => args.push(Arg::Lit(v)),
                            other => args.push(Arg::Expr(other)),
                        }
                    } else if name == "map" && args.is_empty() {
                        // `map(round(3))`, `map(?? 0)`, `map(+ 3)`,
                        // `map(3 + $_)`: the argument is a COMMAND, read
                        // as if a pipe preceded it, the parentheses a
                        // subcontext with the cell as topic; the pipe
                        // may be written (`map(| round(3))`), same
                        // meaning (spec: The Table, "Mapping over the
                        // cells").
                        if matches!(self.peek(), Some(Token::Pipe)) {
                            self.pos += 1;
                        }
                        let saved_depth = self.arg_depth;
                        self.arg_depth = 0;
                        // `map(<int>; --to=:A)`: a trait as the body
                        // redeclares (spec: The Table, draft 166)
                        if matches!(self.peek(), Some(Token::Lt { spaced: false, .. }))
                            && let Some(typing) = self.typing_brackets()?
                        {
                            self.arg_depth = saved_depth;
                            args.push(Arg::Expr(Operand::Typed {
                                inner: Box::new(Operand::Topic),
                                typing,
                            }));
                        } else {
                            let parsed = (|| {
                                let mut commands = Vec::new();
                                // `map(&$.f)`: the held quarq spliced
                                if matches!(self.peek(), Some(Token::Amp)) {
                                    self.invoke_inline_fragment("|", &mut commands)?;
                                } else {
                                    commands.push(self.inline_command()?);
                                }
                                commands.extend(self.inline_pipe_tail()?);
                                Ok::<_, QuarbError>(commands)
                            })();
                            self.arg_depth = saved_depth;
                            let commands = parsed?;
                            args.push(Arg::Expr(Operand::Piped {
                                expr: Box::new(Operand::Seed),
                                commands,
                            }));
                        }
                    } else if matches!(
                        name,
                        "convert" | "unit" | "magnitude" | "exchange" | "dimension"
                    ) && let Some(unit) = self.unit_operand()
                    {
                        // `convert(km/h)`, `unit(m/s^2)`: a bare compound
                        // unit is one operand, the literal's unit grammar
                        // (spec: Quantity Commands).
                        args.push(Arg::Lit(Value::Str(unit)));
                    } else {
                        args.push(self.func_arg()?);
                    }
                    // `;` is the argument separator; `,` is sugar
                    // (spaced, so the decimal comma stays a number).
                    // Trailing, it is the empty last field.
                    if matches!(self.peek(), Some(Token::Comma | Token::Semi)) {
                        self.pos += 1;
                        if matches!(self.peek(), Some(Token::RParen)) {
                            break;
                        }
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
                    // Short switches bundle as the shell bundles them
                    // (draft 164): `-tp` is `-t -p`, in either order;
                    // the last letter keeps the `=value` that follows,
                    // the others take none. Digits never bundle (`-9`
                    // stays the numeric flag), long options never
                    // bundle, and an environment callee's options are
                    // its own to read.
                    let letters: Vec<char> = t[1..].chars().collect();
                    if letters.len() >= 2
                        && letters.iter().all(|c| c.is_alphabetic())
                        && crate::syntax::registry::split_env(command).is_none()
                    {
                        let declared: &[&str] =
                            crate::syntax::registry::spec(command).map_or(&[], |s| s.options);
                        let takes_of = crate::syntax::registry::takes_of(command);
                        for c in &letters {
                            let short = format!("-{c}");
                            if !(declared.contains(&short.as_str()) || (takes_of && short == "-o"))
                            {
                                let mut shorts: Vec<&str> = declared
                                    .iter()
                                    .copied()
                                    .filter(|o| !o.starts_with("--"))
                                    .collect();
                                if takes_of {
                                    shorts.push("-o");
                                }
                                return Err(QuarbError::parse(format!(
                                    "'{short}' in '{t}' is no switch of '{command}' (its short switches: {})",
                                    if shorts.is_empty() {
                                        "none".to_string()
                                    } else {
                                        shorts.join(" ")
                                    }
                                )));
                            }
                        }
                        for c in &letters[..letters.len() - 1] {
                            args.push(Arg::Opt {
                                name: format!("-{c}"),
                                value: None,
                            });
                        }
                        format!("-{}", letters[letters.len() - 1])
                    } else {
                        t
                    }
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
                // `--of=expr` reads a whole expression (`--of=:price
                // * :qty`), the last option it is.
                if matches!(name.as_str(), "--of" | "-o") {
                    Some(self.additive()?)
                } else if name == "--to" && matches!(self.peek(), Some(Token::LBracket)) {
                    // `--to=[rows]`, `--to=[rows]:col` — the value
                    // index of the structure names the cells mapped
                    self.pos += 1;
                    let mut coords = vec![self.index_spec()?];
                    while matches!(self.peek(), Some(Token::Semi | Token::Comma)) {
                        self.pos += 1;
                        coords.push(self.index_spec()?);
                    }
                    self.expect(Token::RBracket, "']' to close the index of --to")?;
                    let (rows, cols) = match coords.len() {
                        1 => (coords.pop().unwrap(), None),
                        2 => {
                            let c = coords.pop().unwrap();
                            (coords.pop().unwrap(), Some(c))
                        }
                        _ => (IndexSpec::Coords(coords), None),
                    };
                    let mut o = Operand::Index {
                        base: Box::new(Operand::Topic),
                        rows,
                        cols,
                    };
                    if let (Some(Token::Field), Some(Token::Name { text, .. })) =
                        (self.peek(), self.toks.get(self.pos + 1))
                    {
                        let name = text.clone();
                        self.pos += 2;
                        o = Operand::Field {
                            base: Box::new(o),
                            name,
                        };
                    }
                    Some(o)
                } else if name == "--to" && matches!(self.peek(), Some(Token::LParen)) {
                    // `--to=(:a; :b)` — a parenthesised projection
                    // list names several columns
                    self.pos += 1;
                    let mut items = Vec::new();
                    loop {
                        items.push(self.unary()?);
                        match self.peek() {
                            Some(Token::Semi | Token::Comma) => self.pos += 1,
                            _ => break,
                        }
                    }
                    self.expect(Token::RParen, "')' to close the column list of --to")?;
                    Some(Operand::List(items))
                } else {
                    Some(self.unary()?)
                }
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
        let declared: &[&str] = crate::syntax::registry::spec(&name).map_or(&[], |s| s.options);
        // Every reducing aggregate takes `--of=expr` (`-o=expr`),
        // the expression read on each item before the reduction
        // (draft 164); it needs its expression.
        let takes_of = crate::syntax::registry::takes_of(&name);
        for arg in &args {
            if let Arg::Opt { name: opt, value } = arg
                && takes_of
                && matches!(opt.as_str(), "--of" | "-o")
            {
                if value.is_none() {
                    return Err(QuarbError::parse(format!(
                        "'{name}({opt}=expr)' takes the expression to reduce: '{name}(--of=:n)'"
                    )));
                }
                continue;
            }
            if let Arg::Opt { name: opt, .. } = arg
                && crate::syntax::registry::split_env(&name).is_none()
                && !declared.contains(&opt.as_str())
            {
                return Err(QuarbError::parse(if takes_of && !declared.is_empty() {
                    format!(
                        "'{name}' takes '--of=expr' ('-o=expr'), the expression to reduce, and the options {}; '{opt}' given",
                        declared.join(" ")
                    )
                } else if takes_of {
                    format!(
                        "'{name}' takes '--of=expr' ('-o=expr'), the expression to reduce; '{opt}' given"
                    )
                } else if declared.is_empty() {
                    format!(
                        "'{name}' takes no options; its parentheses hold operands ('{opt}' given)"
                    )
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
    /// it — or a value alone (auto-named). A grouping key may still
    /// be named by a preceding literal in the flat `k, v` list form
    /// (the Perl-list heritage); for a record that spelling is
    /// retired (2026-10-11), so a bare string in a record is refused
    /// where it was once the name of the field after it.
    fn record_item(&mut self, args: &mut Vec<Arg>, record: bool) -> Result<()> {
        if record && let Some(opt) = self.option_item() {
            // `%(--keys=instant; --values=int)` declares every key and
            // every value (spec: Record, draft 166)
            if matches!(opt.as_str(), "--keys" | "--values") {
                self.pos += 2;
                if !matches!(self.peek(), Some(Token::OptEq)) {
                    return Err(QuarbError::parse(format!(
                        "'{opt}' takes a type word: '%({opt}=int)'"
                    )));
                }
                self.pos += 1;
                let word = match self.bump() {
                    Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) => text.clone(),
                    _ => {
                        return Err(QuarbError::parse(format!(
                            "'{opt}' takes a type word: '%({opt}=int)'"
                        )));
                    }
                };
                if ColumnType::from_word(&word).is_none() {
                    return Err(QuarbError::parse(format!(
                        "'{opt}={word}': '{word}' is no type; the types are {}",
                        ColumnType::words().join(", ")
                    )));
                }
                args.push(Arg::Opt {
                    name: opt,
                    value: Some(Operand::Lit(Value::Str(word))),
                });
                return Ok(());
            }
            return Err(QuarbError::parse(format!(
                "'%(…)' takes no option ('{opt}' given); its items are fields — a hop as a \
                 field's value is written '%(k = (--x))'"
            )));
        }
        if let Some(Token::Name { text, quoted, .. }) = self.peek()
            && matches!(self.toks.get(self.pos + 1), Some(Token::Eq))
        {
            let key = text.clone();
            let quoted = *quoted;
            self.pos += 2;
            // a bare key that reads as a literal — a number, an
            // instant — is a typed key (`%(1 = "a"; 2013-01-01 = 5)`);
            // quoted it stays text ('2013-01-01' another field)
            let typed_key = (record && !quoted)
                .then(|| literal_value(&key, false, &|e| self.expander.unit_scale(e)))
                .map(|v| match v {
                    // an ISO date or instant written bare is an
                    // instant key (`2013-01-01 = 5`)
                    Value::Str(s) => crate::temporal::parse_iso(&s)
                        .map(|(secs, nanos, offset_min)| Value::Instant {
                            secs,
                            nanos,
                            offset_min,
                            calendar: Default::default(),
                        })
                        .unwrap_or(Value::Str(s)),
                    other => other,
                })
                .filter(|v| !matches!(v, Value::Str(_)));
            if let Some(k) = typed_key {
                args.push(Arg::Key(k));
                let value = self.func_arg()?;
                if let Arg::Expr(Operand::Typed { inner, typing }) = &value {
                    return Err(typed_value_refusal(&key, inner, typing.clone()));
                }
                args.push(value);
                return Ok(());
            }
            args.push(Arg::Lit(Value::Str(key.clone())));
            let value = self.func_arg()?;
            if let Arg::Expr(Operand::Typed { inner, typing }) = &value {
                return Err(typed_value_refusal(&key, inner, typing.clone()));
            }
            args.push(value);
            return Ok(());
        }
        // `price<float> = ::price` — the declared field (spec:
        // Record, draft 166): the key with its trait, the value cast
        // into it
        if record && let Some((key, typing)) = self.typed_key()? {
            args.push(Arg::Lit(Value::Str(key)));
            let value = match self.func_arg()? {
                Arg::Expr(e) => e,
                Arg::Lit(v) => Operand::Lit(v),
                _ => {
                    return Err(QuarbError::parse(
                        "a declared field takes a value on the right of '=': %(price<float> = ::price)",
                    ));
                }
            };
            args.push(Arg::Expr(Operand::Typed {
                inner: Box::new(value),
                typing,
            }));
            return Ok(());
        }

        // `@(a; b) = @(1; 2)` — a literal list of names on the left
        // of `=` reads its names bare or quoted, as a header does.
        if record
            && matches!(self.peek(), Some(Token::At))
            && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
            && let Some(close) = self.matching_paren(self.pos + 1)
            && matches!(self.toks.get(close + 1), Some(Token::Eq))
        {
            self.pos += 2;
            let (items, _) = self.list_items(true)?;
            self.pos += 1; // the `=`
            let values = match self.func_arg()? {
                Arg::Expr(e) => e,
                Arg::Lit(Value::List(items)) => {
                    Operand::List(items.iter().cloned().map(Operand::Lit).collect())
                }
                Arg::Lit(v) => Operand::Lit(v),
                _ => {
                    return Err(QuarbError::parse(
                        "the pairwise field takes a list of values on the right of '=': %(@(a; b) = @(1; 2))",
                    ));
                }
            };
            args.push(Arg::Pairs {
                keys: Operand::List(items),
                values,
            });
            return Ok(());
        }
        // `%(:props...)`, `%((fields)...)` — a spread item: the list
        // of projections it reads spreads into fields keyed by their
        // bare keys (spec: The Projection Operator). Tried first; an
        // item that is no spread parses again on the ordinary path.
        if record {
            let save = self.pos;
            if let Ok(Arg::Expr(op)) = self.func_arg()
                && let Some(Token::Name {
                    text,
                    quoted: false,
                    glued: true,
                }) = self.peek()
                && text == "..."
            {
                self.pos += 1;
                args.push(Arg::Expr(Operand::Spread(Box::new(op))));
                return Ok(());
            }
            self.pos = save;
        }
        // `$.k = $.v`, `$.k = $_` — any list expression on the left
        // of `=` takes a list of values pairwise. The left side is
        // read at the additive level, since a single `=` inside an
        // expression would compare; an unnamed item that is no pair
        // is read again the ordinary way.
        if record {
            let save = self.pos;
            if let Ok(keys) = self.additive()
                && matches!(self.peek(), Some(Token::Eq))
            {
                self.pos += 1;
                let values = match self.func_arg()? {
                    Arg::Expr(e) => e,
                    // (a list of literals arrives folded; keep its
                    // constructor so it prints back as one)
                    Arg::Lit(Value::List(items)) => {
                        Operand::List(items.iter().cloned().map(Operand::Lit).collect())
                    }
                    Arg::Lit(v) => Operand::Lit(v),
                    _ => {
                        return Err(QuarbError::parse(
                            "the pairwise field takes a list of values on the right of '=': %(@(a; b) = @(1; 2))",
                        ));
                    }
                };
                args.push(Arg::Pairs { keys, values });
                return Ok(());
            }
            self.pos = save;
        }
        let arg = self.func_arg()?;
        if record && let Arg::Lit(Value::Str(name)) = &arg {
            return Err(QuarbError::parse(format!(
                "a record field needs a name: a leading string no longer names \
                 the field after it (retired); write the key, e.g. \
                 %(name = \"{name}\"; age = 22)"
            )));
        }
        args.push(arg);
        Ok(())
    }

    /// The index of the `)` closing the `(` at `open`, by token
    /// nesting; `None` when none closes it.
    fn matching_paren(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, t) in self.toks.iter().enumerate().skip(open) {
            match t {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
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

    /// Parse an optional trailing projection (`:`, `::`, `:::`).
    fn projection(&mut self) -> Result<Option<Projection>> {
        let proj = match self.peek() {
            // `:name` — the core rung (draft 157: the ladder counts
            // from the core); `:%provenance` the shaped read.
            Some(Token::Field) => {
                self.pos += 1;
                Projection::CoreMeta(canonical_core_field(
                    self.require_projection_name("the core field `:`")?,
                ))
            }
            Some(Token::ColonColon) => {
                self.pos += 1;
                Projection::Property(self.opt_projection_name())
            }
            Some(Token::ColonColonColon) => {
                // on a node path (after a step name, a wildcard, a
                // closed predicate or the root) the rung is the node's
                // metadata and the core-key guard holds; elsewhere (after
                // a pipe, an opening parenthesis, an option's `=`, a
                // separator) it is a descriptor of the value (`| :::index`,
                // `sort_by(:::index)`, `--to=:::index`, `:::name`, draft 166)
                let value_side = self.pos == 0
                    || !matches!(
                        self.toks.get(self.pos - 1),
                        Some(Token::Name { .. } | Token::RBracket | Token::Caret)
                    );
                self.pos += 1;
                let key = self.require_projection_name("adapter metadata `:::`")?;
                adapter_key(key, value_side)?
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
            // `/(*.rs && !test_*)`, `->(a && !b)`: the name algebra
            // on any axis (spec: Name algebra), when the parentheses
            // hold a connective of it; a plain alternation `/(a|b)`
            // stays the path-pattern group it always was.
            if self.name_algebra_ahead() {
                let labels = self.name_selection(&axis)?;
                let mut step = self.finish_step(axis, Matcher::Any)?;
                step.labels = labels;
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
                return Ok(PathElem::Step(step));
            }
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
                labels: Vec::new(),
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
            labels: Vec::new(),
            traits,
            predicates,
            leaf,
        })
    }

    /// Whether the `(` ahead opens a name algebra rather than a
    /// path-pattern group: an `&&` or `||` (or their words), or a
    /// `!` leading a designator, before its closing `)` — outside
    /// the brackets of a predicate and the angles of a trait block,
    /// whose own connectives are theirs. A plain alternation has
    /// none and stays a group.
    fn name_algebra_ahead(&self) -> bool {
        let mut depth = 0usize;
        let mut bracket = 0usize;
        let mut angle = 0usize;
        let mut i = self.pos;
        while let Some(t) = self.toks.get(i) {
            match t {
                Token::LBracket => bracket += 1,
                Token::RBracket => bracket = bracket.saturating_sub(1),
                _ if bracket > 0 => {}
                Token::Lt { .. } => angle += 1,
                Token::Gt { .. } => angle = angle.saturating_sub(1),
                _ if angle > 0 => {}
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                Token::AmpAmp | Token::PipePipe => return true,
                Token::Bang
                    if matches!(
                        self.toks.get(i - 1),
                        Some(Token::LParen | Token::AmpAmp | Token::PipePipe | Token::Pipe)
                    ) =>
                {
                    return true;
                }
                Token::Name {
                    text,
                    quoted: false,
                    ..
                } if (AND_WORDS.contains(&text.as_str())
                    || OR_WORDS.contains(&text.as_str())
                    || NOT_WORDS.contains(&text.as_str()))
                    && matches!(
                        self.toks.get(i + 1),
                        Some(Token::Name { .. } | Token::LParen | Token::Bang)
                    ) =>
                {
                    return true;
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// Parse `(expr)` after a navigation operator: the name algebra
    /// (spec: Name algebra) — `||` or `|` (OR), `&&` (AND), tight `!`
    /// (NOT), parentheses, the designators names, globs and regex
    /// literals — normalized to CNF like a trait filter, one
    /// [`LabelClause`] per conjunct. After a crosslink arrow it is
    /// the label selection, read over the labels.
    fn name_selection(&mut self, axis: &Axis) -> Result<Vec<LabelClause>> {
        self.pos += 1; // consume '('
        let what = if axis.is_link() {
            "a label selection holds label names joined by '&&', '||' and '!': ->(a && !b)"
        } else {
            "a name algebra holds designators — names, globs and regex literals — \
             joined by '&&', '||' and a leading '!': /(*.rs && !test_*)"
        };
        let Some(expr) = self.label_or() else {
            return Err(QuarbError::parse(what));
        };
        if !matches!(self.peek(), Some(Token::RParen)) {
            return Err(match self.peek() {
                Some(Token::Name { .. } | Token::Regex(_)) => QuarbError::parse(format!(
                    "two designators stand joined by '&&' or '||', spaced: {what}"
                )),
                _ => QuarbError::parse(format!("expected ')' closing the parenthesis: {what}")),
            });
        }
        self.pos += 1;
        let clauses = trait_cnf(expr)?;
        clauses
            .into_iter()
            .map(|c| {
                let alts = c
                    .alts
                    .into_iter()
                    .map(|alt| {
                        let (neg, name) = match alt.strip_prefix('!') {
                            Some(n) => (true, n),
                            None => (false, alt.as_str()),
                        };
                        let matcher = if let Some(pat) = name.strip_prefix(REGEX_MARK) {
                            Regex::new(pat).map(Matcher::Regex).map_err(|e| {
                                QuarbError::parse(format!("bad regex '/{pat}/': {e}"))
                            })?
                        } else if let Some(text) = name.strip_prefix(QUOTED_MARK) {
                            Matcher::Name(text.to_string())
                        } else {
                            matcher_for(name, false)?
                        };
                        Ok(LabelAlt { neg, matcher })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(LabelClause { alts })
            })
            .collect()
    }

    fn label_or(&mut self) -> Option<TExpr> {
        let mut left = self.label_and()?;
        while matches!(self.peek(), Some(Token::PipePipe | Token::Pipe))
            || self.eat_word_if_followed(OR_WORDS)
        {
            if matches!(self.peek(), Some(Token::PipePipe | Token::Pipe)) {
                self.pos += 1;
            }
            let right = self.label_and()?;
            left = TExpr::Or(Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn label_and(&mut self) -> Option<TExpr> {
        let mut left = self.label_not()?;
        while matches!(self.peek(), Some(Token::AmpAmp)) || self.eat_word_if_followed(AND_WORDS) {
            if matches!(self.peek(), Some(Token::AmpAmp)) {
                self.pos += 1;
            }
            let right = self.label_not()?;
            left = TExpr::And(Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn label_not(&mut self) -> Option<TExpr> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.pos += 1;
            return Some(TExpr::Not(Box::new(self.label_not()?)));
        }
        if self.eat_word_if_followed(NOT_WORDS) {
            return Some(TExpr::Not(Box::new(self.label_not()?)));
        }
        match self.peek() {
            Some(Token::Name { text, quoted, .. }) => {
                let name = if *quoted {
                    format!("{QUOTED_MARK}{text}")
                } else {
                    text.clone()
                };
                self.pos += 1;
                Some(TExpr::Has(name))
            }
            Some(Token::Regex(pat)) => {
                let pat = format!("{REGEX_MARK}{pat}");
                self.pos += 1;
                Some(TExpr::Has(pat))
            }
            Some(Token::LParen) => {
                self.pos += 1;
                let inner = self.label_or()?;
                if !matches!(self.peek(), Some(Token::RParen)) {
                    return None;
                }
                self.pos += 1;
                Some(inner)
            }
            _ => None,
        }
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
                    | Token::ArrowIn { .. }
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
        // `[2; 4; 5]`, `[1; 3..4; -1]` — a position list: positions
        // and ranges separated by `;`.
        if let Some(parts) = self.position_list()? {
            return Ok(Predicate::Positions(parts));
        }
        let expr = self.pred_or()?;
        self.expect(
            Token::RBracket,
            "']' to close a predicate (')' for the '(?' spelling)",
        )?;
        // Two positions joined by `||` / `&&` select nothing
        // sensible: the position list is the spelling.
        if let PredExpr::Or(a, b) | PredExpr::And(a, b) = &expr
            && let (
                PredExpr::Truthy(Operand::Lit(Value::Int(x))),
                PredExpr::Truthy(Operand::Lit(Value::Int(y))),
            ) = (&**a, &**b)
        {
            return Err(QuarbError::parse(format!(
                "positions are listed with ';': write '[{x}; {y}]'"
            )));
        }
        // Brackets that look at no node — an expression of literals
        // and registers alone — are read at run time as a position
        // or a position list (spec: Index Predicates).
        if let PredExpr::Truthy(op) = &expr
            && node_free(op)
        {
            return Ok(Predicate::Computed(op.clone()));
        }
        Ok(Predicate::Expr(expr))
    }

    /// The position list after a `[`, when the brackets hold one:
    /// two or more bare positions / ranges separated by `;` (a
    /// single one is `Index` / `Range`, parsed by the caller).
    /// Consumes through the `]`; `None` leaves the tokens untouched.
    fn position_list(&mut self) -> Result<Option<Vec<Predicate>>> {
        let start = self.pos;
        let mut parts = Vec::new();
        loop {
            let Some(Token::Name {
                text,
                quoted: false,
                ..
            }) = self.toks.get(self.pos)
            else {
                break;
            };
            let part = if let Ok(n) = text.parse::<i64>() {
                Predicate::Index(n)
            } else if let Some((a, b)) = text.split_once("..") {
                let start = if a.is_empty() { None } else { a.parse().ok() };
                let end = if b.is_empty() { None } else { b.parse().ok() };
                if (a.is_empty() || start.is_some()) && (b.is_empty() || end.is_some()) {
                    Predicate::Range(start, end)
                } else {
                    break;
                }
            } else {
                break;
            };
            parts.push(part);
            self.pos += 1;
            match self.toks.get(self.pos) {
                Some(Token::Semi) => self.pos += 1,
                Some(Token::RBracket) if parts.len() >= 2 => {
                    self.pos += 1;
                    return Ok(Some(parts));
                }
                _ => break,
            }
        }
        self.pos = start;
        Ok(None)
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
        let first = self.pred_and()?;
        self.pred_or_from(first)
    }

    /// The rest of a disjunction after its first conjunction.
    fn pred_or_from(&mut self, first: PredExpr) -> Result<PredExpr> {
        let mut left = first;
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
        let first = self.pred_not()?;
        self.pred_and_from(first)
    }

    /// The rest of a conjunction after its first term.
    fn pred_and_from(&mut self, first: PredExpr) -> Result<PredExpr> {
        let mut left = first;
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
        self.pred_primary_from(left)
    }

    /// A comparison (or chain) whose left side is already parsed —
    /// the topic, after a pipe that opens with the operator.
    fn pred_primary_from(&mut self, left: Operand) -> Result<PredExpr> {
        // after a pipe or an opening parenthesis the operator opens
        // the command (`| > 3`, `map(> 0 ? …)`): no space before it
        let opens = self.pos > 0
            && matches!(
                self.toks.get(self.pos - 1),
                Some(Token::Pipe | Token::LParen)
            );
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
        if !opens
            && let Some(t @ (Token::Gt { before: false, .. } | Token::Lt { before: false, .. })) =
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
                        crate::syntax::unparse::operand_text(&right)
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
            // A further operator continues a chain (spec: Chains in
            // Predicates): `a R b S c` is `a R b && b S c`, every
            // operand evaluated once, any comparison operators.
            let mut operands = vec![left, right];
            let mut ops = vec![op];
            while let Some(next) = self.cmp_op() {
                ops.push(next);
                operands.push(self.additive()?);
            }
            if ops.len() == 1 {
                let right = operands.pop().unwrap();
                let left = operands.pop().unwrap();
                return Ok(PredExpr::Compare(left, op, right));
            }
            Ok(PredExpr::Chain(operands, ops))
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
        let left = self.multiplicative()?;
        self.additive_from(left)
    }

    /// The additive loop from a left operand already read. The
    /// concatenation operators `++`, `++!` and `+@+` stand at this
    /// level with `+` and `-`, left-associative (spec: Concatenation).
    fn additive_from(&mut self, mut left: Operand) -> Result<Operand> {
        loop {
            if let Some(mode) = self.concat_word_ahead() {
                left = self.concat_rest(mode, left)?;
                continue;
            }
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
                // `a -::b`: the minus glued to a projection is the
                // descending key marker, never a one-sided minus
                Some(Token::Desc) => {
                    return Err(QuarbError::parse(
                        "a binary minus is spaced on both sides: write 'a - b' (a glued '-::key' is the descending sort key)",
                    ));
                }
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

    /// A concatenation operator at the cursor: the spaced word `++`
    /// (`++!` with the bang glued, as `+|!` was) or `+@+`.
    fn concat_word_ahead(&self) -> Option<ConcatMode> {
        let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
        else {
            return None;
        };
        match text.as_str() {
            "++" if matches!(self.toks.get(self.pos + 1), Some(Token::Bang)) => {
                Some(ConcatMode::Overwrite)
            }
            "++" => Some(ConcatMode::Merge),
            "+@+" => Some(ConcatMode::Item),
            _ => None,
        }
    }

    /// The concatenation after its operator: the right side at the
    /// multiplicative level, or `%(…)` as a row under the table
    /// literal's rules (positional by the left side's header, named,
    /// or both).
    fn concat_rest(&mut self, mode: ConcatMode, left: Operand) -> Result<Operand> {
        self.pos += if mode == ConcatMode::Overwrite { 2 } else { 1 };
        let right = if matches!(self.peek(), Some(Token::Percent))
            && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
        {
            self.pos += 2;
            Operand::Row(self.record_row()?)
        } else {
            self.multiplicative()?
        };
        Ok(Operand::Concat {
            mode,
            left: Box::new(left),
            right: Box::new(right),
        })
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
            (_, _) if numeric(text) => {
                match crate::syntax::unparse::operand_text(left).chars().last() {
                    Some(c @ ('+' | '-' | '*')) => c,
                    _ => return Ok(()),
                }
            }
            _ => return Ok(()),
        };
        Err(QuarbError::parse(format!(
            "'{op}' is an operator and takes a space on both sides: write ' {op} '"
        )))
    }

    /// `unary ((*|div|idiv|mod) unary)*`. A bare `*` here follows a
    /// complete operand, so it is multiplication, not the wildcard.
    fn multiplicative(&mut self) -> Result<Operand> {
        let left = self.unary()?;
        self.multiplicative_from(left)
    }

    /// The multiplicative loop from a left operand already read.
    fn multiplicative_from(&mut self, mut left: Operand) -> Result<Operand> {
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
                // `x` — the matrix product, at the level of `*`.
                Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) if text == "x" => ArithOp::MatMul,
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
        // `VALUE-->` — a node reference resolved to its node (spec:
        // The Capsa, "Node references"): `$.Name-->`, `$.Name-->/kid::x`.
        // The value is the id anchor's expression; the path goes on
        // from the node it names. A path operand keeps its own
        // resolution axis (`$$.e-->ref`, `::ref-->`).
        if matches!(self.peek(), Some(Token::Resolve))
            && matches!(
                o,
                Operand::Recall(_) | Operand::Field { .. } | Operand::Index { .. }
            )
            && !matches!(
                self.toks.get(self.pos + 1),
                Some(Token::Name { glued: true, .. })
            )
        {
            self.pos += 1;
            if let Operand::Rel {
                steps, projection, ..
            } = self.rel_operand_continue(Vec::new())?
            {
                o = Operand::Rel {
                    steps,
                    projection,
                    anchor: Anchor::Id(Box::new(o)),
                };
            }
        }
        // `X:.r` — the cross-capsa read: a register
        // accessor glued to a node path reads the register of the
        // capsa standing at the node. The node side must be a bare
        // node operand — a path, `$$_`, `$$N` — without a projection
        // (a projected path is a value, and a value has no capsa).
        if let Some(Token::Capsa(body)) = self.peek() {
            let body = body.clone();
            let reg = match body.as_str() {
                "." => RegRef::Top,
                "@." => RegRef::Whole,
                "@@." => RegRef::NodeWhole,
                "%." => RegRef::Record,
                "%.*" => RegRef::FullRecord,
                "%%." => RegRef::NodeRecord,
                "%%.*" => RegRef::NodeFullRecord,
                "%.." | "%%.." => {
                    return Err(QuarbError::parse(format!(
                        "':{body}' is retired: the full view is ':{}.*'",
                        body.trim_end_matches('.')
                    )));
                }
                b if b.starts_with(['@', '%']) => {
                    return Err(QuarbError::parse(format!(
                        "':{b}' is not a register view: the views are :@. :@@. :%. :%.* :%%. :%%.*"
                    )));
                }
                b => reg_ref(&b[1..])?,
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
                Operand::Invoking(inner) | Operand::Position(inner) | Operand::Driver(inner) => {
                    bare(inner)
                }
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
        loop {
            match self.peek() {
                // `v:@(…)`, `v:%(…)`, `t:#(…)`, `v:(…)`, `t:$.proj` —
                // the projection operator (spec: The Projection
                // Operator): the shape applied to the value.
                Some(Token::ProjectColon) => {
                    self.pos += 1;
                    let shape = self.project_shape()?;
                    o = Operand::Projected {
                        base: Box::new(o),
                        shape,
                    };
                }
                Some(Token::Field) => {
                    self.pos += 1;
                    let name = match self.bump() {
                        Some(Token::Name { text, .. }) => text.clone(),
                        _ => return Err(QuarbError::parse("expected a field name after ':'")),
                    };
                    // On a bare node path the single colon is the
                    // core rung (`/a/x:name`, `(-->):name`).
                    if let Operand::Rel {
                        projection: projection @ None,
                        ..
                    } = &mut o
                    {
                        *projection = Some(Projection::CoreMeta(canonical_core_field(name)));
                        continue;
                    }
                    // `:%provenance` / `:@origin` on a value: its
                    // provenance in the record or the list shape.
                    if name.starts_with(['%', '@']) {
                        o = Operand::ValueMeta {
                            base: Box::new(o),
                            key: name,
                        };
                        continue;
                    }
                    o = Operand::Field {
                        base: Box::new(o),
                        name,
                    };
                }
                // `t[[label]]`, `t[[x; y]]`, `t[[a..b]]`, `r[[k]]` — the
                // double bracket looks up by label (spec: Indexing a
                // Value, draft 166)
                Some(Token::LBracket)
                    if matches!(self.toks.get(self.pos + 1), Some(Token::LBracket))
                        && !matches!(
                            o,
                            Operand::Rel { .. } | Operand::Ctx { .. } | Operand::Pattern(_)
                        ) =>
                {
                    let labels = self.label_brackets()?;
                    o = Operand::LabelIndex {
                        base: Box::new(o),
                        labels,
                    };
                }
                // `value[i]`, `value[a..b]`, `table[i; j]` — brackets
                // after a value expression index it (spec: Indexing a
                // Value); a path step took its own predicates already.
                Some(Token::LBracket)
                    if !matches!(
                        o,
                        Operand::Rel { .. } | Operand::Ctx { .. } | Operand::Pattern(_)
                    ) =>
                {
                    self.pos += 1;
                    let mut coords = vec![self.index_spec()?];
                    while matches!(self.peek(), Some(Token::Semi | Token::Comma)) {
                        self.pos += 1;
                        coords.push(self.index_spec()?);
                    }
                    self.expect(Token::RBracket, "']' to close the index")?;
                    // one coordinate, or two (`t[i; j]`, the cell);
                    // three or more are a position list on a list
                    let (rows, cols) = match coords.len() {
                        1 => (coords.pop().unwrap(), None),
                        2 => {
                            let c = coords.pop().unwrap();
                            (coords.pop().unwrap(), Some(c))
                        }
                        _ => (IndexSpec::Coords(coords), None),
                    };
                    o = Operand::Index {
                        base: Box::new(o),
                        rows,
                        cols,
                    };
                }
                _ => break,
            }
        }
        // `value:::key` — a descriptor of the value, glued (draft
        // 166): `$,df:::index`, `$_:::dim`, `t:::header`, what the
        // pipe form `| :::key` answers, the value piped through it.
        // Any other key after a value is retired (draft 157): a
        // value's provenance reads through the shape sigils only. A
        // node path parsed its own `:::` as the adapter rung already.
        while matches!(self.peek(), Some(Token::ColonColonColon)) {
            if let Some(Token::Name {
                text,
                quoted: false,
                ..
            }) = self.toks.get(self.pos + 1)
                && matches!(
                    text.as_str(),
                    "fields"
                        | "keys"
                        | "header"
                        | "type"
                        | "dim"
                        | "nrows"
                        | "ncols"
                        | "index"
                        | "name"
                )
            {
                let key = text.clone();
                self.pos += 2;
                o = Operand::Piped {
                    expr: Box::new(o),
                    commands: vec![Command::Expr(Operand::Rel {
                        steps: Vec::new(),
                        projection: Some(Projection::AdapterMeta(key)),
                        anchor: Anchor::Current,
                    })],
                };
                continue;
            }
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
                    "':::key' after a value is retired: its provenance reads as ':%provenance' (the record; ':%provenance:source' for one key), ':@provenance', ':%origin' or ':@origin'",
                ));
            }
            self.pos += 1;
            let key = self.require_projection_name("value provenance `:::`")?;
            return Err(QuarbError::parse(format!(
                "':::{key}' after a value is retired: its provenance reads as ':%provenance:{key}' (the record), ':@provenance', ':%origin' or ':@origin'"
            )));
        }
        // `:A<int>`, `::price<float>`, `:A<*>` — the typed projector
        // (spec: Projectors, draft 166): the trait brackets glued to a
        // bare projection declare the field it names.
        if is_bare_projection(&o)
            && let Some(typing) = self.typing_brackets()?
        {
            o = Operand::Typed {
                inner: Box::new(o),
                typing,
            };
        }
        Ok(o)
    }

    /// `(:first; :second)` — a parenthesised list of projectors, the
    /// cursor on `(`: the composite index's levels, folded to the
    /// literal list of projectors they spell.
    fn projector_list(&mut self) -> Result<Operand> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            let mut item = self.unary()?;
            head_projections(&mut item);
            items.push(item);
            match self.peek() {
                Some(Token::Semi | Token::Comma) => self.pos += 1,
                _ => break,
            }
        }
        self.expect(Token::RParen, "')' to close the list of index columns")?;
        if items.iter().all(|i| matches!(i, Operand::Lit(_))) {
            let values = items
                .into_iter()
                .map(|i| match i {
                    Operand::Lit(v) => v,
                    _ => unreachable!(),
                })
                .collect();
            return Ok(Operand::Lit(Value::list(values)));
        }
        Ok(Operand::List(items))
    }

    /// `[[x; y]]`, `[[a..b]]` — the labels of a double-bracket lookup
    /// (spec: Indexing a Value, draft 166), the cursor on the first
    /// `[`; each label the literal it spells, a range of two both
    /// ends in.
    fn label_brackets(&mut self) -> Result<Vec<Operand>> {
        self.pos += 2;
        let mut labels = Vec::new();
        loop {
            if matches!(self.peek(), Some(Token::RBracket)) {
                break;
            }
            // a bare word reads as the literal it spells (an ISO date
            // the instant); a quoted label is text
            let bare = matches!(self.peek(), Some(Token::Name { quoted: false, .. }));
            let label = self.additive()?;
            labels.push(if bare {
                label_range(label, &|e| self.expander.unit_scale(e))
            } else {
                label
            });
            match self.peek() {
                Some(Token::Semi | Token::Comma) => self.pos += 1,
                Some(Token::RBracket) => {}
                _ => {
                    return Err(QuarbError::parse(
                        "the double bracket holds labels apart by ';': t[[x; y]], a range t[[a..b]]",
                    ));
                }
            }
        }
        if labels.is_empty() {
            return Err(QuarbError::parse(
                "the double bracket names a label: t[[2013-01-02]], r[[\"a\"]]",
            ));
        }
        self.pos += 1;
        if !matches!(self.peek(), Some(Token::RBracket)) {
            return Err(QuarbError::parse("the double bracket closes with ']]'"));
        }
        self.pos += 1;
        Ok(labels)
    }

    /// The trait brackets of a typed projector, glued: `<int>`,
    /// `<*>`, `<*; float>` (spec: Projectors, draft 166). `None`
    /// without consuming where no bracket opens here; a word that is
    /// no type is refused naming the types.
    fn typing_brackets(&mut self) -> Result<Option<crate::value::Typing>> {
        use crate::value::Typing;
        if !matches!(self.peek(), Some(Token::Lt { spaced: false, .. })) {
            return Ok(None);
        }
        let start = self.pos;
        self.pos += 1;
        let word = |t: Option<&Token>| match t {
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) => Some(text.clone()),
            _ => None,
        };
        // `<(int; text)>` — the tuple type, its words in parentheses
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            let mut words = Vec::new();
            loop {
                let Some(w) = word(self.peek()) else {
                    return Err(QuarbError::parse(
                        "a tuple type lists its types in parentheses: '<(int; text)>'",
                    ));
                };
                self.pos += 1;
                words.push(w);
                match self.peek() {
                    Some(Token::Semi | Token::Comma) => self.pos += 1,
                    _ => break,
                }
            }
            self.expect(Token::RParen, "')' to close the tuple type")?;
            let spelled = format!("({})", words.join("; "));
            let ty = ColumnType::from_word(&spelled).ok_or_else(|| {
                QuarbError::parse(format!(
                    "'<{spelled}>': a tuple's types are {}",
                    ColumnType::words().join(", ")
                ))
            })?;
            if !matches!(self.peek(), Some(Token::Gt { .. })) {
                return Err(QuarbError::parse(
                    "the trait brackets of a projector close with '>'",
                ));
            }
            self.pos += 1;
            return Ok(Some(Typing::Declared(ty)));
        }
        let Some(first) = word(self.peek()) else {
            self.pos = start;
            return Ok(None);
        };
        self.pos += 1;
        let typing = if first == "*" {
            let reported = if matches!(self.peek(), Some(Token::Semi | Token::Comma)) {
                self.pos += 1;
                let Some(w) = word(self.peek()) else {
                    return Err(QuarbError::parse(
                        "'<*; type>' reports the inferred type after the star: '<*; float>'",
                    ));
                };
                self.pos += 1;
                Some(ColumnType::from_word(&w).ok_or_else(|| {
                    QuarbError::parse(format!(
                        "'<*; {w}>': '{w}' is no type; the types are {}",
                        ColumnType::words().join(", ")
                    ))
                })?)
            } else {
                None
            };
            Typing::Star(reported)
        } else {
            match ColumnType::from_word(&first) {
                Some(t) => Typing::Declared(t),
                None => {
                    if matches!(self.peek(), Some(Token::Gt { .. })) {
                        return Err(QuarbError::parse(format!(
                            "'<{first}>' on a projector declares its type, and '{first}' is none; the types are {}; '<*>' declares nothing",
                            ColumnType::words().join(", ")
                        )));
                    }
                    self.pos = start;
                    return Ok(None);
                }
            }
        };
        if !matches!(self.peek(), Some(Token::Gt { .. })) {
            return Err(QuarbError::parse(format!(
                "the trait brackets of a projector close with '>': '{}'",
                typing.spelling()
            )));
        }
        self.pos += 1;
        Ok(Some(typing))
    }

    /// `name<type> =` — a record key written with its trait (spec:
    /// Record, draft 166): the key and its typing, consumed with the
    /// `=`; `None` without consuming otherwise.
    fn typed_key(&mut self) -> Result<Option<(String, crate::value::Typing)>> {
        let start = self.pos;
        let Some(Token::Name { text, .. }) = self.peek() else {
            return Ok(None);
        };
        if !matches!(
            self.toks.get(self.pos + 1),
            Some(Token::Lt { spaced: false, .. })
        ) {
            return Ok(None);
        }
        let key = text.clone();
        self.pos += 1;
        let typing = match self.typing_brackets() {
            Ok(Some(t)) if matches!(self.peek(), Some(Token::Eq)) => t,
            Ok(_) => {
                self.pos = start;
                return Ok(None);
            }
            Err(e) => {
                if self.toks[start + 1..self.pos.max(start + 1)]
                    .iter()
                    .any(|t| matches!(t, Token::Gt { .. }))
                    || matches!(self.toks.get(self.pos), Some(Token::Gt { .. }))
                {
                    return Err(e);
                }
                self.pos = start;
                return Ok(None);
            }
        };
        self.pos += 1; // the `=`
        Ok(Some((key, typing)))
    }

    fn unary_primary(&mut self) -> Result<Operand> {
        // `1..5`, `0..8..2`, `1..(n + 1)`: the range, glued around
        // its dots; the first bound rides the token, the rest may
        // be parenthesized expressions.
        if let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.peek()
            && let Some((from, rest)) = range_head(text)
        {
            self.pos += 1;
            return self.range_rest(Operand::Lit(Value::Int(from)), rest);
        }
        // `x` is the matrix product, a reserved word of the value
        // grammar: it stands between two operands and nowhere else
        // (the string is written quoted; names behind a sigil are
        // untouched).
        if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "x")
            && self.toks.get(self.pos + 1).is_none_or(|t| {
                !matches!(
                    t,
                    Token::RParen | Token::Semi | Token::Comma | Token::RBracket | Token::Pipe
                )
            })
        {
            return Err(QuarbError::parse(
                "'x' is the matrix product and stands between two operands; the string is written quoted (\"x\")",
            ));
        }
        if matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == "-") {
            self.pos += 1;
            return Ok(Operand::Neg(Box::new(self.unary()?)));
        }
        // `-::fare` — the glued minus on a projection: the
        // descending key marker (spec: Keyed Aggregates).
        if matches!(self.peek(), Some(Token::Desc)) {
            self.pos += 1;
            return Ok(Operand::Desc(Box::new(self.unary()?)));
        }
        // `-:col` opening a keyed aggregate's argument: the
        // descending marker on a core field (the incoming arrow is
        // written pointy there, `<-col`; the rounded `-:` stays the
        // arrow everywhere else).
        if self.keyed_args
            && self.pos > 0
            && matches!(
                self.toks.get(self.pos - 1),
                Some(Token::LParen | Token::Semi | Token::Comma)
            )
            && let (
                Some(Token::ArrowIn { rounded: true }),
                Some(Token::Name {
                    text,
                    quoted: false,
                    glued: true,
                }),
            ) = (self.toks.get(self.pos), self.toks.get(self.pos + 1))
        {
            let name = text.clone();
            self.pos += 2;
            return Ok(Operand::Desc(Box::new(Operand::Field {
                base: Box::new(Operand::Topic),
                name,
            })));
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
            // A parenthesised operand is a body, in a command's
            // parentheses too (`tree(0; (fields))`,
            // `project((fields); -r)`): inside it the expression
            // grammar applies and a bare word names a command, where
            // among the command's own operands it is a string.
            let saved_depth = self.arg_depth;
            self.arg_depth = 0;
            let parsed = (|| {
                let inner = self.cond_expr()?;
                // An operand may carry a pipe tail inside its parens:
                // `(expr | f @| g)` — command semantics, mirrored.
                let commands = self.inline_pipe_tail()?;
                Ok::<_, QuarbError>((inner, commands))
            })();
            self.arg_depth = saved_depth;
            let (inner, commands) = parsed?;
            self.expect(Token::RParen, "')' to close a group")?;
            // `(n)..5` — a parenthesized lower bound of a range.
            if commands.is_empty()
                && let Some(Token::Name {
                    text,
                    quoted: false,
                    ..
                }) = self.peek()
                && let Some(rest) = text.strip_prefix("..")
                && rest
                    .split("..")
                    .all(|p| p.is_empty() || p.parse::<i64>().is_ok())
            {
                let rest: Vec<String> = rest.split("..").map(str::to_string).collect();
                self.pos += 1;
                return self.range_rest(inner, rest);
            }
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

    /// The bounds of a range after its first: each an integer that
    /// rode the token, or an empty part standing for a parenthesized
    /// expression that follows; a step may ride the next token
    /// (`1..(n)..2`). One bound is the end, two the end and the step.
    fn range_rest(&mut self, from: Operand, mut parts: Vec<String>) -> Result<Operand> {
        let mut bounds: Vec<Operand> = Vec::new();
        let mut i = 0;
        while i < parts.len() {
            let p = parts[i].clone();
            i += 1;
            if p.is_empty() {
                if !matches!(self.peek(), Some(Token::LParen)) {
                    return Err(QuarbError::parse(
                        "a range's bound is an integer or a parenthesized expression: 1..5, 1..(n + 1)",
                    ));
                }
                self.pos += 1;
                let e = self.additive()?;
                self.expect(Token::RParen, "')' to close the range's bound")?;
                bounds.push(e);
                // the step may ride the next token: `..2`, `..(s)`
                if i == parts.len()
                    && bounds.len() == 1
                    && let Some(Token::Name {
                        text,
                        quoted: false,
                        ..
                    }) = self.peek()
                    && let Some(rest) = text.strip_prefix("..")
                    && (rest.is_empty() || rest.parse::<i64>().is_ok())
                {
                    parts.push(rest.to_string());
                    self.pos += 1;
                }
            } else {
                let n: i64 = p.parse().map_err(|_| {
                    QuarbError::parse(format!("a range's bound is an integer: '{p}' is not one"))
                })?;
                bounds.push(Operand::Lit(Value::Int(n)));
            }
        }
        let mut bounds = bounds.into_iter();
        let to = bounds
            .next()
            .ok_or_else(|| QuarbError::parse("a range needs its end: 1..5"))?;
        let step = bounds.next().map(Box::new);
        if bounds.next().is_some() {
            return Err(QuarbError::parse(
                "a range has at most three parts: start..end..step",
            ));
        }
        Ok(Operand::Range {
            from: Box::new(from),
            to: Box::new(to),
            step,
        })
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
        if crate::syntax::registry::split_env(&name).is_none()
            && crate::syntax::registry::spec(&name).is_none()
            && retired_command(&name).is_none()
        {
            return Err(QuarbError::parse(format!(
                "'{name}' is not a command; a bare word in a body names one: \
                 quote a string \"{name}\""
            )));
        }
        // A prefixed command keeps its own parentheses in a body
        // (`%(pos = morph:pos(--lang=ru))`): operands and options
        // of the command, the seed its input. A built-in with
        // parentheses is the call operand, read before this arm
        // (`upper(x)` is `(x | upper)`).
        let call = if crate::syntax::registry::split_env(&name).is_some()
            && matches!(self.peek(), Some(Token::LParen))
        {
            self.call_with_name(name)?
        } else {
            FnCall {
                name,
                args: Vec::new(),
            }
        };
        let mut commands = vec![func_command(call)?];
        commands.extend(self.inline_pipe_tail()?);
        Ok(Operand::Piped {
            expr: Box::new(Operand::Seed),
            commands,
        })
    }

    /// `a ?? b` after a boolean-level expression: the null default,
    /// grouping to the right (`a ?? b ?? c` is `a ?? (b ?? c)`), below
    /// arithmetic and comparison, above the conditional, so
    /// `(a + b ?? c ? x : y)` is `((a + b) ?? c) ? x : y`.
    fn coalesce_tail(&mut self, left: PredExpr) -> Result<PredExpr> {
        if !matches!(self.peek(), Some(Token::Coalesce)) {
            return Ok(left);
        }
        self.pos += 1;
        let right = self.pred_or()?;
        let right = self.coalesce_tail(right)?;
        let as_operand = |p: PredExpr| match p {
            PredExpr::Truthy(op) => op,
            other => Operand::Group(Box::new(other)),
        };
        Ok(PredExpr::Truthy(Operand::Coalesce {
            left: Box::new(as_operand(left)),
            right: Box::new(as_operand(right)),
        }))
    }

    fn cond_expr(&mut self) -> Result<Operand> {
        let cond = self.pred_or()?;
        let cond = self.coalesce_tail(cond)?;
        self.cond_expr_from(cond)
    }

    /// The conditional after its condition: a value match (`?= k ?
    /// r : else`), the plain `? a : b`, or the condition alone.
    fn cond_expr_from(&mut self, cond: PredExpr) -> Result<Operand> {
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
                pred @ (Predicate::Index(_)
                | Predicate::Range(_, _)
                | Predicate::Positions(_)
                | Predicate::Computed(_)) => Ok(Command::Select(pred)),
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
                let head = self.pos;
                self.pos += 1;
                let enriched = if matches!(self.peek(), Some(Token::Percent)) {
                    self.pos += 1;
                    true
                } else {
                    false
                };
                if matches!(self.peek(), Some(Token::LParen)) {
                    let call = self.record_args()?;
                    // `@| %(v = $_) ++ %(w = 1)` — the constructor
                    // heads an expression over the context.
                    if !enriched && self.expr_continues() {
                        self.pos = head;
                        return Ok(Some(Command::AggExpr(self.additive()?)));
                    }
                    validate_record(&call)?;
                    return Ok(Some(Command::AggRecord { call, enriched }));
                }
                // `@| %,` — the session memory as a record (draft 166)
                if !enriched
                    && matches!(self.peek(), Some(Token::Name { text, quoted: false, .. }) if text == ",")
                {
                    self.pos += 1;
                    return Ok(Some(Command::AggRecall(RegRef::SessionRecord)));
                }
                // `@| %` bare — the column reading of the stream.
                if !enriched && !self.at_view_dots() {
                    return Ok(Some(Command::Agg(FnCall {
                        name: "%".into(),
                        args: Vec::new(),
                    })));
                }
                let dots = self.view_dots(if enriched { "%%" } else { "%" })?;
                Ok(Some(Command::AggRecall(match (enriched, dots) {
                    (false, 1) => RegRef::Record,
                    (false, _) => RegRef::FullRecord,
                    (true, 1) => RegRef::NodeRecord,
                    (true, _) => RegRef::NodeFullRecord,
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
            Some(Token::At) if matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, .. }) if text == ".") =>
            {
                self.pos += 2;
                Ok(Some(Command::AggRecall(RegRef::Whole)))
            }
            Some(Token::AtAt) if matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, .. }) if text == ".") =>
            {
                self.pos += 2;
                Ok(Some(Command::AggRecall(RegRef::NodeWhole)))
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
                        | Token::ArrowIn { .. }
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
    /// bash operator — `expr:?` / `expr:?message` (the strict expansion)
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
                        | Token::ArrowIn { .. }
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
                                | Token::ArrowIn { .. }
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
                                | Token::ArrowIn { .. }
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
            // parent's name with `rec(dir, \*:name)`. The bare
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
                | Token::ArrowIn { .. }
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
                                | Token::ArrowIn { .. }
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
            Some(Token::ColonColon | Token::ColonColonColon) => {
                let projection = self.projection()?.expect("projection start");
                Ok(Operand::Rel {
                    steps: Vec::new(),
                    projection: Some(projection),
                    anchor: Anchor::Current,
                })
            }
            // `q(cmd)` — the quarq literal (spec: Quarqs, draft 166):
            // the body, a command as it stands after a pipe (the pipe
            // may be written), read as Quarb and not run. Its value is
            // the quarq — or the projector the body spells when it
            // spells nothing else (`q(::x)`, `q(%(::Name; ::Fare))`),
            // a projector being the simplest quarq.
            Some(Token::Name {
                text,
                quoted: false,
                ..
            }) if text == "q" && matches!(self.toks.get(self.pos + 1), Some(Token::LParen)) => {
                self.pos += 2;
                if matches!(self.peek(), Some(Token::Pipe)) {
                    self.pos += 1;
                }
                let saved_depth = self.arg_depth;
                let saved_keyed = self.keyed_args;
                self.arg_depth = 0;
                self.keyed_args = false;
                let saved_sub = self.subquery_depth;
                self.subquery_depth = 0;
                // the body may navigate (`q(//item[::n > 3])`, spliced
                // later with `| &$.p`) and push: it is a pipeline's
                // tail, read as the pipeline would read it
                let parsed = (|| {
                    let mut commands = Vec::new();
                    if matches!(self.peek(), Some(Token::Amp)) {
                        self.invoke_pipeline_fragment("|", &mut commands)?;
                    } else {
                        commands.push(self.pipe_item(PipeMode::Nav)?);
                    }
                    let mode = commands
                        .iter()
                        .fold(PipeMode::Nav, |m, c| command_mode_out(c, m));
                    self.pipeline_items(&mut commands, mode)?;
                    Ok::<_, QuarbError>(commands)
                })();
                self.arg_depth = saved_depth;
                self.keyed_args = saved_keyed;
                self.subquery_depth = saved_sub;
                let commands = parsed?;
                self.expect(Token::RParen, "')' to close the quarq")?;
                Ok(Operand::Lit(quarq_literal(commands)))
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
                    Some(Arg::Range(..))
                    | Some(Arg::Key(_))
                    | Some(Arg::Named { .. })
                    | Some(Arg::Opt { .. })
                    | Some(Arg::Pairs { .. }) => {
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
                // A reducing callee takes the values its first
                // argument reaches gathered into one list, as its
                // input would be (`sum(/*::n)` = 6, `count(/*)` = 3);
                // a scalar callee takes them one at a time and forks
                // (`upper(/*::k)`) — draft 164.
                let first = if crate::syntax::registry::takes_of(&command_call.name) {
                    Operand::Gathered(Box::new(first))
                } else {
                    first
                };
                self.check_unit_call(&command_call)?;
                let command = func_command_at(command_call, true)?;
                Ok(Operand::Piped {
                    expr: Box::new(first),
                    commands: vec![command],
                })
            }
            Some(Token::Name { text, quoted, .. }) => {
                let value = literal_value(text, *quoted, &|e| self.expander.unit_scale(e));
                // In a body a bare word names a command applied to
                // the body's seed (`.n(cc)`, `%(total = count)`);
                // an unknown one is refused with its name, never
                // read as text. A command's own parentheses are the one
                // exception (`stem(ru)`): there it is the short
                // spelling of a quoted string.
                if !*quoted
                    && self.arg_depth == 0
                    && is_word(text)
                    && matches!(value, Value::Str(_))
                {
                    let mut name = text.clone();
                    self.pos += 1;
                    // `lib:name` — a library's or an environment's
                    // command stands headless in a body too
                    // (`%(lemma = morph:lemma)`).
                    if let (
                        Some(Token::Field | Token::Colon { spaced: false }),
                        Some(Token::Name {
                            text: cmd,
                            quoted: false,
                            glued: true,
                        }),
                    ) = (self.peek(), self.toks.get(self.pos + 1))
                    {
                        name = format!("{name}:{cmd}");
                        self.pos += 2;
                    }
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
            // `%,` — the session memory as a record, a value wherever
            // a register stands (draft 166)
            Some(Token::Percent) if matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, quoted: false, .. }) if text == ",") =>
            {
                self.pos += 2;
                Ok(Operand::Recall(RegRef::SessionRecord))
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
            // `#(…)` — the table literal; `#(--width=n)` /
            // `#(--header=list)` the reshape of the topic where it
            // stands, as `%(…)` reads its topic there.
            Some(Token::Hash) => {
                if let Some(err) = self.retired_table_count() {
                    return Err(err);
                }
                if self.table_option_ahead() {
                    let call = self.table_options()?;
                    return Ok(Operand::Piped {
                        expr: Box::new(Operand::Topic),
                        commands: vec![Command::Func(call)],
                    });
                }
                self.pos += 1;
                return self.table_literal();
            }
            // The context family (spec: Context Accessors): `@_` the
            // topics, `@@_` the positions with an optional glued
            // projection, `#*` / `##*` the context as a table.
            Some(Token::Topics) => {
                self.pos += 1;
                Ok(Operand::Topics)
            }
            Some(Token::Positions) => {
                self.pos += 1;
                let projection = self.projection()?;
                Ok(Operand::Positions { projection })
            }
            Some(Token::Matrix { full }) => {
                let full = *full;
                self.pos += 1;
                Ok(Operand::Matrix { node: false, full })
            }
            Some(Token::NodeMatrix { full }) => {
                let full = *full;
                self.pos += 1;
                Ok(Operand::Matrix { node: true, full })
            }
            Some(Token::CtxTable) => {
                self.pos += 1;
                Ok(Operand::CtxTable)
            }
            // `@-` — all arrived-by edges (bare: labels; projected:
            // each edge's property); `@.` — the value regulae; `@@.`
            // — the node references; `@(…)` — the list literal.
            Some(Token::AtAt) if matches!(self.toks.get(self.pos + 1), Some(Token::Name { text, quoted: false, .. }) if text == ".") =>
            {
                self.pos += 2;
                Ok(Operand::Recall(RegRef::NodeWhole))
            }
            Some(Token::At) => {
                self.pos += 1;
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.pos += 1;
                    let (items, _) = self.list_items(false)?;
                    return Ok(Operand::List(items));
                }
                // `@%(…)` — the record array, a list of records in
                // the table literal's shape.
                if matches!(self.peek(), Some(Token::Percent))
                    && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
                {
                    self.pos += 1;
                    return self.record_array();
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
                    }) if text == "*" => Err(QuarbError::parse(
                        "'@*' is retired: the context is '@_' (the topics), '@@_' \
                         (the positions; '@@_::prop' maps over them), '#*' (the \
                         table of the value regulae) or '##*' (of the node regulae)",
                    )),
                    _ => Err(QuarbError::parse(
                        "expected '-' (arrived edges), '.' (register), '_' (the \
                         topics) or '@_' (the positions) after '@' in an operand",
                    )),
                }
            }
            // The cursor operands, projected or navigated like any
            // node (`$$_::prop`, `$$__/child::x`); bare, the node
            // itself as a reference: `$$_` the position, `$$__` the
            // invoking position, `$$0` the driver.
            Some(Token::Position) => {
                self.pos += 1;
                return self.position();
            }
            // `_` — the cursor record; `_:topic` is `$_`, `_:position`
            // is `$$_` with its tail, any other `_:k` a field miss.
            Some(Token::Cursor) => {
                self.pos += 1;
                if matches!(
                    self.peek(),
                    Some(Token::ColonColon | Token::ColonColonColon)
                ) {
                    return Err(QuarbError::parse(CURSOR_PROJECTION));
                }
                if matches!(self.peek(), Some(Token::Field)) {
                    match self.toks.get(self.pos + 1) {
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text == "position" => {
                            self.pos += 2;
                            return self.position();
                        }
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text == "topic" => {
                            self.pos += 2;
                            return Ok(Operand::Topic);
                        }
                        _ => {}
                    }
                }
                return Ok(Operand::Cursor);
            }
            Some(Token::Invoking) => {
                self.pos += 1;
                return self.invoking_position();
            }
            Some(Token::Driver) => {
                self.pos += 1;
                return self.driver();
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
                                // `$$0` — the driver.
                                return self.driver();
                            }
                            joined = Some(k);
                        }
                        None => {
                            return Err(QuarbError::parse(
                                "the capsa is shared: '$.name', '$_' and '$ord' read the same register \
                                 inside a body or a join condition; '$$_' is the position, '$$__' the \
                                 invoking position, '$$0' the driver, '$$N' a bound node, '$$.name' a mark",
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
                            return Ok(Operand::Recall(reg_ref(&rest)?));
                        }
                        // `$,name` — the session read (draft 166)
                        Some(Token::Name {
                            text,
                            quoted: false,
                            ..
                        }) if text.starts_with(',') => {
                            let rest = text[1..].to_string();
                            self.pos += 1;
                            if rest.is_empty() {
                                return Err(QuarbError::parse(
                                    "the session read names its value: '$,df' (the capsa's register is '$.df')",
                                ));
                            }
                            return Ok(Operand::Recall(RegRef::Session(rest)));
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
            None => Err(QuarbError::parse(
                "the query ends where a value is expected: a binary operator takes a right \
                 side (12 x @(1; 2)); a quantity literal is written 12_h, a text \"12 h\"",
            )),
            other => Err(QuarbError::parse(format!(
                "expected a value or path in a predicate, found {other:?}"
            ))),
        }
    }

    /// Expand a fragment invocation in operand position.
    /// The register after `&`: `&$.p` splices the quarq it holds.
    fn splice_register(&mut self) -> Result<RegRef> {
        match self.operand()? {
            Operand::Recall(r) => Ok(r),
            _ => Err(QuarbError::parse(
                "'&' before a register splices the quarq it holds: &$.p, &$.",
            )),
        }
    }

    fn splice_operand_fragment(&mut self) -> Result<Operand> {
        // `&$.p` as an operand: the held quarq run on the capsa, as
        // a piped body would be (spec: Quarqs)
        if matches!(self.toks.get(self.pos + 1), Some(Token::Dollar)) {
            self.pos += 1;
            let r = self.splice_register()?;
            return Ok(Operand::Piped {
                expr: Box::new(Operand::Seed),
                commands: vec![Command::Splice(r)],
            });
        }
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
                let invited = invites_of(self.expander, &args);
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
                self.record_captures(&name, &invited, self.expander.usage_of_operand(&op));
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

    /// The shape after the projection operator's colon: `#(…)` the
    /// table of the named columns (bare projections or `name =
    /// expr`), else a constructor, a parenthesised expression or a
    /// register, read as an operand.
    fn project_shape(&mut self) -> Result<ProjShape> {
        let saved_depth = self.arg_depth;
        self.arg_depth = 0;
        let shape = if matches!(self.peek(), Some(Token::Hash))
            && matches!(self.toks.get(self.pos + 1), Some(Token::LParen))
        {
            self.pos += 2;
            self.shape_columns()
        } else {
            // a typed projector in the shape is the typed value, so
            // the shape applied casts the column it reads (`t:@(:A<int>)`)
            self.unary().map(|mut v| {
                fold_typed(&mut v);
                ProjShape::Value(Box::new(v))
            })
        };
        self.arg_depth = saved_depth;
        shape
    }

    /// An expression heading a command (`| $_ + 1`, `| :n ?? 0`):
    /// the additive expression with a null-default tail.
    fn expr_head(&mut self) -> Result<Operand> {
        let left = self.additive()?;
        self.expr_head_from(left)
    }

    /// The rest of an expression head after its first operand.
    fn expr_head_from(&mut self, left: Operand) -> Result<Operand> {
        // a comparison, a boolean connective, a null default or a
        // conditional may follow (`| $_ = null`, `| $_ > 3 ? $_ :
        // null`, `| $_ ?? 0`): the full expression
        if self.cmp_ahead()
            || matches!(
                self.peek(),
                Some(
                    Token::Coalesce
                        | Token::AmpAmp
                        | Token::PipePipe
                        | Token::Question { .. }
                        | Token::QuestionEq
                )
            )
        {
            let first = self.pred_primary_from(left)?;
            let first = self.pred_and_from(first)?;
            let cond = self.pred_or_from(first)?;
            let cond = self.coalesce_tail(cond)?;
            return self.cond_expr_from(cond);
        }
        Ok(left)
    }

    /// Whether a comparison operator stands next (without taking it).
    fn cmp_ahead(&mut self) -> bool {
        let save = self.pos;
        let ahead = self.cmp_op().is_some();
        self.pos = save;
        ahead
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        // after a pipe or an opening parenthesis the operator opens
        // the command (`| > 3`, `map(> 0 ? …)`): nothing stands
        // before it to be spaced from
        let opens = self.pos > 0
            && matches!(
                self.toks.get(self.pos - 1),
                Some(Token::Pipe | Token::LParen)
            );
        let op = match self.peek()? {
            Token::Eq => CmpOp::Eq,
            Token::Ne => CmpOp::Ne,
            Token::Lt {
                spaced: true,
                before,
            } if *before || opens => CmpOp::Lt,
            Token::Le => CmpOp::Le,
            Token::Gt {
                spaced: true,
                before,
            } if *before || opens => CmpOp::Gt,
            Token::Ge => CmpOp::Ge,
            Token::MatchEq => CmpOp::Match,
            Token::NotMatchEq => CmpOp::NotMatch,
            Token::Contains => CmpOp::Contains,
            Token::Within => CmpOp::Within,
            Token::Encloses => CmpOp::Encloses,
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

    /// The tail of a cursor operand after its token: an optional
    /// relative path and projection (`$$__::prop`, `$$_/child::x`),
    /// else the node itself.
    fn cursor_tail(&mut self) -> Result<Box<Operand>> {
        let mut steps = Vec::new();
        while matches!(self.peek(), Some(Token::Slash | Token::SlashSlash)) {
            steps.push(self.path_elem()?);
        }
        let projection = self.projection()?;
        Ok(Box::new(Operand::Rel {
            steps,
            projection,
            anchor: Anchor::Current,
        }))
    }

    /// `$$__` — the invoking position.
    fn invoking_position(&mut self) -> Result<Operand> {
        Ok(Operand::Invoking(self.cursor_tail()?))
    }

    /// `$$_` — the position.
    fn position(&mut self) -> Result<Operand> {
        Ok(Operand::Position(self.cursor_tail()?))
    }

    /// `$$0` — the driver, legal inside a join condition only: the
    /// enclosing correlation is lexically decidable, so the miss is
    /// a parse error rather than a silent null.
    fn driver(&mut self) -> Result<Operand> {
        if self.correlation_depth == 0 {
            if !self.in_def_body {
                return Err(QuarbError::parse(DRIVER_PLACEMENT));
            }
            // Inside a body, outside any join of the body's own:
            // the splice site decides.
            self.body_uses_driver = true;
        }
        Ok(Operand::Driver(self.cursor_tail()?))
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
                // name rather than reading a field. The segment
                // wildcards ride the same glue: `:*` lexes as a
                // glued colon and the name `*`, `:.` as the
                // cross-capsa read `.` — here both are one more
                // segment (`<tag:*>`, `<tag:.>`, `<a:.:b>`).
                loop {
                    match (self.peek(), self.toks.get(self.pos + 1)) {
                        (
                            Some(Token::Field | Token::Colon { spaced: false }),
                            Some(Token::Name { text, .. }),
                        ) => {
                            let text = text.clone();
                            self.pos += 2;
                            name.push(':');
                            name.push_str(&text);
                        }
                        (Some(Token::Capsa(c)), _) if c == "." => {
                            self.pos += 1;
                            name.push_str(":.");
                        }
                        _ => break,
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
            Some(Token::ArrowIn { .. }) => Axis::InLink,
            Some(Token::DashDash) => Axis::BothLink,
            Some(Token::Name { text, .. }) => {
                return Err(QuarbError::parse(format!(
                    "expected a navigation operator before '{text}' \
                     (queries are root-anchored; start with '/', or open \
                     with an expression head: '= expr')"
                )));
            }
            // `:name` spaced after a step: a core field glues to it.
            Some(Token::Field) => {
                let name = match self.peek() {
                    Some(Token::Name { text, .. }) => text.clone(),
                    _ => "name".to_string(),
                };
                return Err(QuarbError::parse(format!(
                    "':{name}' stands apart from its step: a core field glues to it \
                     ('/x:{name}'; ':fields' lists them), a property is '::{name}'"
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
            // a bare `/` before a pipe or at the end: the root alone
            // is the anchor `^` (`^ | …` is what `= …` sugars)
            Some(Token::Pipe) | None => Err(QuarbError::parse(
                "a navigation operator must be followed by a name; the root alone is the \
                 anchor '^' ('^ | …')",
            )),
            _ => Err(QuarbError::parse(
                "a navigation operator must be followed by a name",
            )),
        }
    }
}

/// A token that opens a range (`1..5`, `1..`, `0..8..2`, `1..5..`):
/// its first bound, and the parts after it (an empty part stands for
/// a parenthesized expression that follows the token).
fn range_head(text: &str) -> Option<(i64, Vec<String>)> {
    let (first, rest) = text.split_once("..")?;
    let from: i64 = first.parse().ok()?;
    let parts: Vec<String> = rest.split("..").map(str::to_string).collect();
    if parts.len() > 2
        || parts
            .iter()
            .any(|p| !p.is_empty() && p.parse::<i64>().is_err())
    {
        return None;
    }
    Some((from, parts))
}

/// The arithmetic operator a bare word spells, for the operator in
/// after a pipe.
fn arith_word(text: &str) -> Option<ArithOp> {
    Some(match text {
        "+" => ArithOp::Add,
        "-" => ArithOp::Sub,
        "*" => ArithOp::Mul,
        "div" => ArithOp::Div,
        "idiv" => ArithOp::IDiv,
        "mod" => ArithOp::Mod,
        "x" => ArithOp::MatMul,
        _ => return None,
    })
}

/// The marks a name algebra's designators carry through the CNF
/// normalizer, whose literals are strings: a regex designator's
/// body, and a quoted name (taken literally, never as a glob).
/// Neither character occurs in a designator.
const REGEX_MARK: char = '\u{1}';
const QUOTED_MARK: char = '\u{2}';

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
    if crate::syntax::registry::split_env(&call.name).is_some() {
        return Ok(Command::Agg(call));
    }
    let Some(spec) = crate::syntax::registry::spec(&call.name).filter(|s| s.agg.is_some()) else {
        return Err(QuarbError::unsupported(format!(
            "aggregate command '{}'",
            call.name
        )));
    };
    if call.name == "ungroup" && !call.args.is_empty() {
        return Err(QuarbError::parse("'ungroup' takes no arguments"));
    }
    check_operand_bound(&call, false, true)?;
    // The shape checks see the operands; `--of=expr` is read apart.
    let shaped = if crate::syntax::registry::takes_of(&call.name) {
        call.without_of()
    } else {
        call.clone()
    };
    match spec.check {
        crate::syntax::registry::Check::Command => validate_window_shift(&shaped)?,
        crate::syntax::registry::Check::Keyed => validate_keyed(&shaped)?,
        _ => {}
    }
    Ok(Command::Agg(call))
}

/// A `|` command: a scalar, or an aggregate that also works per capsa
/// — a keyed aggregate on a group's members (`@| group(::k) | top(2,
/// ::v)`), a reducing one on a list topic (`@| group(::k) | mean`).
/// A whole-context command (`ungroup`, `window`, `shift`) never takes
/// the plain pipe. The arguments pass the registry's check.
/// The refusal of a unit operand nobody knows, in the command's
/// words: `convert(KB): no unit 'KB' (kB is 1000 B, KiB is 1024 B)`.
fn unit_operand_refusal(command: &str, operand: &str, names: &[String]) -> Option<String> {
    crate::value::quantity::unit_operand_refusal(operand, names)
        .map(|why| format!("{command}({operand}): {why}"))
}

/// The adjacency rule (spec: Pipelines, "Stdin"): a built-in after
/// a pipe reads its input from the pipe and never names it, so a
/// surplus operand is refused with the synopsis — and when the
/// surplus is `$_`, the pointer says where the input comes from.
/// Inside an expression the first argument rides as the input, and
/// the rest is held to the same bound. Environment callees (`py:`,
/// `sh:`) keep the `$_` input-slot placement, and a library's own
/// gate counts its operands.
fn check_operand_bound(call: &FnCall, nested: bool, agg: bool) -> Result<()> {
    if !call.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Ok(());
    }
    let Some(max) = crate::syntax::man::operand_bound(&call.name) else {
        return Ok(());
    };
    // The retired positional word `sample` points at its switch
    // before any count is held against it (draft 164).
    if crate::syntax::registry::spec(&call.name).is_some_and(|s| s.options.contains(&"--sample"))
        && call
            .args
            .iter()
            .any(|a| matches!(a, Arg::Lit(Value::Str(s)) if s == "sample"))
    {
        return Err(QuarbError::parse(format!(
            "'sample' is an option: write {0}(--sample)",
            call.name
        )));
    }
    // A reducing aggregate reads the field to reduce through its
    // `--of=expr` option (draft 164: `@| group(::k) | sum(--of=:n)`);
    // a key written as an operand is a surplus operand, refused with
    // the synopsis, and `$_` among them is the input misplaced.
    let spec = crate::syntax::registry::spec(&call.name);
    let reducing =
        spec.is_some_and(|s| matches!(s.agg, Some(crate::syntax::registry::Agg::Reducing)));
    // A keyed aggregate (`sort_by`, `group`, `top`, `unique_by`)
    // takes `$_` as a key — the topic itself — never as the input
    // placed: a surplus there is a count, not a misplaced input.
    let keyed_agg =
        spec.is_some_and(|s| matches!(s.agg, Some(crate::syntax::registry::Agg::Keyed)));
    let places_input = !keyed_agg
        && call
            .args
            .iter()
            .any(|a| matches!(a, Arg::Expr(Operand::Topic)));
    let is_operand = |a: &Arg| !matches!(a, Arg::Opt { .. } | Arg::Named { .. });
    let written = call.args.iter().filter(|a| is_operand(a)).count();
    if written <= max && !(reducing && places_input && !nested) {
        return Ok(());
    }
    let synopsis = crate::syntax::man::synopsis(&call.name).unwrap_or_default();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    if !nested && places_input {
        let stripped = FnCall {
            name: call.name.clone(),
            args: call
                .args
                .iter()
                .filter(|a| !matches!(a, Arg::Expr(Operand::Topic)))
                .cloned()
                .collect(),
        };
        let command = if agg {
            Command::Agg(stripped)
        } else {
            Command::Func(stripped)
        };
        return Err(QuarbError::parse(format!(
            "{} reads its input from the pipe: write {}",
            call.name,
            crate::syntax::unparse::command(&command)
        )));
    }
    let takes = match max {
        0 => "no operand".to_string(),
        n => format!("{n} operand{} at most", plural(n)),
    };
    Err(QuarbError::parse(if nested {
        format!(
            "'{}' inside an expression takes its input and {takes} after it, {written} given; the synopsis: {synopsis}",
            call.name
        )
    } else {
        format!(
            "'{}' takes {takes}, {written} given; the synopsis: {synopsis}",
            call.name
        )
    }))
}

/// The canonical spelling of a core field (draft 164): `:props` for
/// `:properties`, `:meta` for `:metadata`; unparse prints the short
/// forms.
fn canonical_core_field(name: String) -> String {
    match name.as_str() {
        "properties" => "props".to_string(),
        "metadata" => "meta".to_string(),
        _ => name,
    }
}

fn func_command(call: FnCall) -> Result<Command> {
    func_command_at(call, false)
}

fn func_command_at(call: FnCall, nested: bool) -> Result<Command> {
    use crate::syntax::registry::{Agg, Check};
    // An environment command runs once per capsa after `|`.
    if crate::syntax::registry::split_env(&call.name).is_some() {
        return Ok(Command::Func(call));
    }
    check_operand_bound(&call, nested, false)?;
    let spec = crate::syntax::registry::spec(&call.name);
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
        // `| 12h`: a number glued to a unit is a bare word, a
        // command lookup; the quantity literal takes the underscore.
        let digits: String = call
            .name
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let unit = &call.name[digits.len()..];
        if !digits.is_empty()
            && digits.starts_with(|c: char| c.is_ascii_digit())
            && !unit.is_empty()
            && unit.chars().all(char::is_alphabetic)
            && !matches!(
                literal_value(&format!("{digits}_{unit}"), false, &|_| None),
                Value::Str(_)
            )
        {
            return Err(err.with_hint(format!(
                " (a bare word after a pipe names a command; the quantity literal is written \
                 {digits}_{unit}, the text \"{}\")",
                call.name
            )));
        }
        return Err(if spec.is_some_and(|s| s.agg == Some(Agg::Context)) {
            err.with_hint(format!(" ('{}' uses '@|')", call.name))
        } else {
            err
        });
    }
    // The shape checks see the operands; `--of=expr` is read apart.
    let shaped = if crate::syntax::registry::takes_of(&call.name) {
        call.without_of()
    } else {
        call.clone()
    };
    match spec.map(|s| s.check) {
        Some(Check::Keyed) => validate_keyed(&shaped)?,
        _ => validate_record(&shaped)?,
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
                if crate::value::encoding::is_decodable(&v.to_string())
                    || crate::value::encoding::is_structured_format(&v.to_string()) =>
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
        CmpOp::Within => "<<=",
        CmpOp::Encloses => ">>=",
    }
}

/// The retired command names, each with its pointer: `rec` /
/// `record` (the record sigil `%(…)`) and `chars`
/// (the pre-0.25 spelling of `cc`).
pub(crate) fn retired_command(name: &str) -> Option<String> {
    match name {
        "rec" | "record" => Some(format!("'{name}(…)' is retired: the record is '%(…)'")),
        "chars" => Some("'chars' is retired: write 'cc'".into()),
        "types" => Some("'types' is retired: the type of a table's columns is 'type'".into()),
        "table" => Some("'table' is retired: the table is '@| pretty'".into()),
        // retired at 0.42.0 (spec: Projectors, the projection operator)
        "project" => Some(
            "'project(…)' is retired: the projection operator — 'v:%(…)', 't:#(:A; :B)', 'v:$.p' — and '| %' for a node's record"
                .into(),
        ),
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
                 explicitly: other-{name} = …"
            )));
        }
        names.push(name.to_string());
        Ok(())
    };
    let mut i = 0;
    while i < call.args.len() {
        match &call.args[i] {
            // A literal string names the following argument (the
            // parser's spelling of `key = value`); a typed key (draft
            // 166) likewise.
            Arg::Lit(Value::Str(name)) => {
                if i + 1 >= call.args.len() {
                    return Err(QuarbError::parse(format!(
                        "{what} has a trailing field name with no value"
                    )));
                }
                check(name)?;
                i += 2;
            }
            Arg::Key(_) => {
                if i + 1 >= call.args.len() {
                    return Err(QuarbError::parse(format!(
                        "{what} has a trailing field name with no value"
                    )));
                }
                i += 2;
            }
            // `--keys=T` / `--values=T` declare the fields (draft 166)
            Arg::Opt { name, .. } if matches!(name.as_str(), "--keys" | "--values") => {
                i += 1;
            }
            // a spread item names its fields by their projections
            Arg::Expr(Operand::Spread(_)) => {
                i += 1;
            }
            Arg::Expr(e) => {
                let Some(name) = crate::syntax::ast::auto_field_name(e) else {
                    return Err(QuarbError::parse(format!(
                        "a {what} field needs a name: give a computed value a key, \
                         e.g. %(total = ::price * ::qty)"
                    )));
                };
                let name = name.to_string();
                check(&name)?;
                i += 1;
            }
            // The pairwise fields: a literal list of names is
            // checked here; computed names at run time.
            Arg::Pairs { keys, .. } => {
                if let Operand::List(items) = keys {
                    for item in items {
                        if let Operand::Lit(v) = item {
                            check(&v.to_string())?;
                        }
                    }
                }
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
        // `--desc` is an option, not a locale or mode word
        let positional: Vec<&Arg> = call
            .args
            .iter()
            .filter(|a| !matches!(a, Arg::Opt { .. }))
            .collect();
        match positional.as_slice() {
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
    // the options (`--desc`) are not keys
    let mut keys = args.filter(|a| !matches!(a, Arg::Opt { .. })).peekable();
    // a quarq written as a key is applied on each capsa (spec: Quarqs)
    if keys.peek().is_none()
        || keys.any(|a| matches!(a, Arg::Lit(v) if !matches!(v, Value::Quarq(_))))
    {
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
        // the switches (--nulls / --values) stand beside the operand
        "frequencies" => match call
            .args
            .iter()
            .filter(|a| !matches!(a, Arg::Opt { .. }))
            .collect::<Vec<_>>()
            .as_slice()
        {
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
        // `csv[(a; b; …)]` / `pretty[(a; b; …)]` — the columns by name.
        "csv" | "table" | "pretty" => {
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
                "number takes an optional level expression: number, number(:depth)",
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
        // `tree[(operands; switches)]` — a depth and column
        // expressions in any order, then the display switches
        // `--traits` (`-t`), `--props` (`-p`), `--record` (`-r`)
        // (draft 164: a bare word in a parenthesis is a string, so
        // the former words refuse with the switch).
        "tree" => {
            for a in &call.args {
                if let Arg::Lit(Value::Str(s)) = a {
                    let switch = match s.as_str() {
                        "kind" => Some("--kind or -k"),
                        "traits" => Some("--traits or -t"),
                        "props" => Some("--props or -p"),
                        "%" | "record" => Some("--record or -r"),
                        _ => None,
                    };
                    return Err(QuarbError::parse(match switch {
                        Some(sw) => format!("'{s}' is a string here: write {sw}"),
                        None => format!(
                            "tree takes a depth and column expressions, then the switches \
                             --kind, --traits, --props, --record: tree(2; -t), tree(0; -r; ::mtime) \
                             ('{s}' is a string here)"
                        ),
                    }));
                }
            }
            let ok = call.args.iter().all(|a| match a {
                Arg::Lit(Value::Int(n)) => *n >= 0,
                Arg::Expr(_) | Arg::Opt { .. } => true,
                _ => false,
            }) && call
                .args
                .iter()
                .filter(|a| matches!(a, Arg::Lit(Value::Int(_))))
                .count()
                <= 1;
            if ok {
                Ok(())
            } else {
                Err(QuarbError::parse(
                    "tree takes a depth and column expressions, then the switches \
                     --kind (-k), --traits (-t), --props (-p), --record (-r): tree(2; -t), \
                     tree(0; -r; ::mtime), tree(0; -t; (/* @| count))",
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
                 style: outline(:depth; ::lemma), outline(:depth; ::lemma; \"1.\")",
            )),
        },
        // `zscore([key]; [--sample])` — the switch selects the n − 1
        // spread (draft 164; the positional word is retired), an
        // expression partitions the standardizing.
        "zscore" => {
            let operands: Vec<&Arg> = call
                .args
                .iter()
                .filter(|a| !matches!(a, Arg::Opt { .. }))
                .collect();
            if operands
                .iter()
                .any(|a| matches!(a, Arg::Lit(Value::Str(s)) if s == "sample"))
            {
                return Err(QuarbError::parse(
                    "'sample' is an option: write zscore(--sample), zscore(key; --sample)",
                ));
            }
            let ok = match operands.as_slice() {
                [] => true,
                [Arg::Expr(_)] => true,
                _ => false,
            };
            if ok {
                Ok(())
            } else {
                Err(QuarbError::parse(
                    "zscore takes a partition key and the switch --sample: \
                     zscore, zscore($.w), zscore(--sample), zscore($.w; --sample)",
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
            // The options (`--of=expr`) are not the word; a leading
            // expression is left to the operand bound's refusal.
            let operands: Vec<&Arg> = call
                .args
                .iter()
                .filter(|a| !matches!(a, Arg::Opt { .. }))
                .collect();
            let rest = operands.as_slice();
            // The positional word is retired (draft 164): the n − 1
            // spread is the switch; the field to reduce is `--of`.
            if rest
                .iter()
                .any(|a| matches!(a, Arg::Lit(Value::Str(s)) if s == "sample"))
            {
                return Err(QuarbError::parse(format!(
                    "'sample' is an option: write {0}(--sample)",
                    call.name
                )));
            }
            if rest.is_empty() {
                Ok(())
            } else {
                Err(QuarbError::parse(format!(
                    "{0} takes no operand, the switch --sample and --of=expr: {0}, {0}(--sample), {0}(--sample --of=:x)",
                    call.name
                )))
            }
        }
        // `encoding(LABEL)` — the output boundary: one label the
        // Encoding Standard knows. The command stands last; the tool
        // that prints applies it (the engine has no byte value).
        "encoding" => match call.args.as_slice() {
            [Arg::Lit(Value::Str(label))] => {
                if crate::value::encoding::encoding_for_label(label).is_some() {
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
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

pub(crate) fn literal_value(
    text: &str,
    quoted: bool,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Value {
    if quoted {
        return Value::Str(text.to_string());
    }
    // `5_km`, `12_EUR/kg` — the quantity literal (spec: Physical
    // Quantities), read with the source's units; a pure-time unit
    // is a Duration; a unit nothing reads leaves the text.
    if crate::syntax::lexer::is_quantity_head(text) {
        if let Some((value, base, wv, wu)) =
            crate::value::quantity::parse_unit_text_with(text, scale)
        {
            return Value::Quantity {
                value,
                base,
                written: Some((wv, wu)),
            };
        }
        let glued = text.replacen('_', "", 1);
        if let Some((secs, nanos)) = Value::Str(glued.clone())
            .durational_reading()
            .or_else(|| crate::temporal::span_from_units(&glued, scale))
        {
            return Value::seconds(secs, nanos);
        }
        return Value::Str(text.to_string());
    }
    match text {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        // the float literals a printed value reads back as (spec:
        // Numbers, draft 166)
        "nan" => return Value::Float(f64::NAN),
        "inf" => return Value::Float(f64::INFINITY),
        "-inf" => return Value::Float(f64::NEG_INFINITY),
        _ => {}
    }
    // an ISO date or instant written bare is the instant literal
    // (spec: Numbers, draft 166: a literal of any kind)
    if text.len() >= 10
        && text.as_bytes()[4] == b'-'
        && let Some((secs, nanos, offset_min)) = crate::temporal::parse_iso(text)
    {
        return Value::Instant {
            secs,
            nanos,
            offset_min,
            calendar: Default::default(),
        };
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
/// The adapter rung with a core key is refused: the core names are
/// reserved on the adapter level (spec: Metadata, draft 157), so an
/// old `:::name` points at `:name` instead of reading metadata.
fn adapter_key(key: String, value_side: bool) -> Result<Projection> {
    // `:::index` is a table's labels and `:::name` its header's name
    // (draft 166), value-side forms: the guard that points a core
    // property back to `:` does not fire on them after a pipe (nor
    // glued to a value, which never reaches it)
    if crate::syntax::ast::CORE_KEYS.contains(&key.as_str())
        && !(value_side && matches!(key.as_str(), "index" | "name"))
    {
        return Err(QuarbError::parse(format!(
            "':::{key}' is the adapter metadata, and '{key}' is a core property: write ':{key}' (the ladder counts from the core: ':name', '::p', ':::k')"
        )));
    }
    Ok(Projection::AdapterMeta(key))
}

fn is_projection_start(tok: &Token) -> bool {
    matches!(tok, Token::ColonColon | Token::ColonColonColon)
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
        Command::SessionPush(_) => {}
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
        Command::Expr(e)
        | Command::AggExpr(e)
        | Command::ExprPush { expr: e, .. }
        | Command::AggPush { body: e, .. } => subst_operand(e, map),
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
        | Command::Splice(_)
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
        PredExpr::Chain(operands, _) => operands.iter_mut().for_each(|o| subst_operand(o, map)),
        PredExpr::Truthy(o) => subst_operand(o, map),
    }
}

fn subst_operand(o: &mut Operand, map: &Subst<'_>) {
    match o {
        // A pattern literal holds no operands: nothing to splice.
        Operand::Pattern(_) => {}
        // A quarq's body is source: a def's parameters are spliced
        // into it as into any body, and the literal is rebuilt.
        Operand::Lit(Value::Quarq(q)) => {
            let mut body = (*q.body).clone();
            body.iter_mut().for_each(|c| subst_command(c, map));
            *o = Operand::Lit(quarq_literal(body));
        }
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
        Operand::Arith { left, right, .. } | Operand::Concat { left, right, .. } => {
            subst_operand(left, map);
            subst_operand(right, map);
        }
        Operand::Row(items) => {
            for (_, i) in items {
                subst_operand(i, map);
            }
        }
        Operand::Neg(inner)
        | Operand::Desc(inner)
        | Operand::Gathered(inner)
        | Operand::Spread(inner) => subst_operand(inner, map),
        Operand::Projected { base, shape } => {
            subst_operand(base, map);
            match shape {
                ProjShape::Value(v) => subst_operand(v, map),
                ProjShape::Columns { items, index } => {
                    for (_, item) in items {
                        subst_operand(item, map);
                    }
                    if let Some(i) = index {
                        subst_operand(i, map);
                    }
                }
            }
        }
        Operand::Typed { inner, .. } => subst_operand(inner, map),
        Operand::LabelIndex { base, labels } => {
            subst_operand(base, map);
            for l in labels {
                subst_operand(l, map);
            }
        }
        Operand::Field { base, .. }
        | Operand::ValueMeta { base, .. }
        | Operand::Index { base, .. } => subst_operand(base, map),
        Operand::Coalesce { left, right } => {
            subst_operand(left, map);
            subst_operand(right, map);
        }
        Operand::PeerReg { at, .. } => subst_operand(at, map),
        Operand::List(items) => {
            for item in items {
                subst_operand(item, map);
            }
        }
        Operand::RecordArray { rows, .. } => {
            for row in rows {
                for (_, i) in row {
                    subst_operand(i, map);
                }
            }
        }
        Operand::Range { from, to, step } => {
            subst_operand(from, map);
            subst_operand(to, map);
            if let Some(s) = step {
                subst_operand(s, map);
            }
        }
        Operand::Table { rows, .. } => {
            for row in rows {
                match row {
                    TableRow::List(items) => {
                        for i in items {
                            subst_operand(i, map);
                        }
                    }
                    TableRow::Record(items) => {
                        for (_, i) in items {
                            subst_operand(i, map);
                        }
                    }
                }
            }
        }
        Operand::NamedCaptures => {}
        Operand::Group(e) => subst_pred_expr(e, map),
        Operand::Invoking(inner) | Operand::Position(inner) | Operand::Driver(inner) => {
            subst_operand(inner, map)
        }
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
        | Operand::Cursor
        | Operand::Seed
        | Operand::Ordinal
        | Operand::Edge { .. }
        | Operand::Edges { .. }
        | Operand::Topics
        | Operand::Positions { .. }
        | Operand::Matrix { .. }
        | Operand::CtxTable
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

/// At the expression head (`= expr`) every bare projection — `:k`,
/// `::k`, `:::k` — becomes the projector it spells (spec: The
/// Projection Operator, draft 166: the head has no cursor), through
/// the constructors and operators that hold it; a projection
/// operator's shape is left to be read on its base, and a path with
/// steps stays a path.
/// The quarq literal from its body (spec: Quarqs): the quarq, holding
/// the projector or structured projector the body spells when it is
/// nothing but projectors and literals in a list or a record —
/// `q(::x)` answers the projector `::x`, `q(%(::Name; ::Fare))` the
/// structured projector the `=` head gives. A read, a register, a
/// command or a pipe in the body leaves it a quarq, applied later;
/// the body is kept as written.
fn quarq_literal(commands: Vec<Command>) -> Value {
    use crate::syntax::ast::{Anchor, Projection};
    let projector = match commands.as_slice() {
        [Command::Expr(op)] => {
            let mut folded = op.clone();
            head_projections(&mut folded);
            fold_closed(&folded)
        }
        // `q(::x)` read as the pipeline reads `| ::x`: the projection
        // alone on the cursor
        [Command::Nav(b)] if b.steps.is_empty() && matches!(b.anchor, Anchor::Current) => {
            match &b.projection {
                Some(Projection::CoreMeta(k)) => Some(Value::projection(1, k.clone())),
                Some(Projection::Property(Some(k))) => Some(Value::projection(2, k.clone())),
                Some(Projection::AdapterMeta(k)) => Some(Value::projection(3, k.clone())),
                _ => None,
            }
        }
        // `q(%(::Name; ::Fare))` read as the pipeline reads `| %(…)`
        [Command::Func(call)] if matches!(call.name.as_str(), "rec" | "record") => {
            let mut args = call.args.clone();
            for a in &mut args {
                if let Arg::Expr(e) = a {
                    head_projections(e);
                }
            }
            fold_record(&args)
        }
        _ => None,
    }
    .filter(Value::holds_projector);
    Value::quarq(commands, projector)
}

/// A constant operand's value: a literal, a list of constants, a
/// record constructor of constants; anything read at run time is
/// `None`.
fn fold_closed(op: &Operand) -> Option<Value> {
    match op {
        Operand::Lit(v) => Some(v.clone()),
        Operand::List(items) => Some(Value::list(
            items.iter().map(fold_closed).collect::<Option<Vec<_>>>()?,
        )),
        Operand::Piped { expr, commands }
            if matches!(**expr, Operand::Topic | Operand::Seed)
                && let [Command::Func(call)] = commands.as_slice()
                && matches!(call.name.as_str(), "rec" | "record") =>
        {
            fold_record(&call.args)
        }
        _ => None,
    }
}

/// A record constructor's constant value from its arguments (the
/// record convention: `name = value` pairs, bare projections keyed
/// by their own name); `None` where a field is read at run time.
fn fold_record(args: &[Arg]) -> Option<Value> {
    let mut fields = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg {
            Arg::Lit(Value::Str(name)) => {
                let value = match args.next()? {
                    Arg::Expr(e) => fold_closed(e)?,
                    Arg::Lit(v) => v.clone(),
                    _ => return None,
                };
                fields.push((name.clone(), value));
            }
            Arg::Expr(e) => {
                let name = crate::syntax::ast::auto_field_name(e)?.to_string();
                fields.push((name, fold_closed(e)?));
            }
            _ => return None,
        }
    }
    Some(Value::Record(fields.into()))
}

fn head_projections(op: &mut Operand) {
    use crate::syntax::ast::{Anchor, Projection, TableRow};
    if let Operand::Typed { inner, typing } = op {
        let typing = typing.clone();
        head_projections(inner);
        if let Operand::Lit(Value::Projection { ladder, key, .. }) = inner.as_ref() {
            *op = Operand::Lit(Value::typed_projection(*ladder, key.clone(), Some(typing)));
        }
        return;
    }
    let spelled = match op {
        Operand::Field { base, name } if matches!(**base, Operand::Topic) => {
            Some(Value::projection(1, name.clone()))
        }
        Operand::Rel {
            steps,
            projection: Some(p),
            anchor: Anchor::Current,
        } if steps.is_empty() => match p {
            Projection::CoreMeta(k) => Some(Value::projection(1, k.clone())),
            Projection::Property(Some(k)) => Some(Value::projection(2, k.clone())),
            Projection::AdapterMeta(k) => Some(Value::projection(3, k.clone())),
            Projection::Property(None) => None,
        },
        _ => None,
    };
    if let Some(v) = spelled {
        *op = Operand::Lit(v);
        return;
    }
    match op {
        Operand::List(items) => items.iter_mut().for_each(head_projections),
        Operand::Table { rows, .. } => {
            for row in rows {
                match row {
                    TableRow::List(items) => items.iter_mut().for_each(head_projections),
                    TableRow::Record(items) => {
                        items.iter_mut().for_each(|(_, o)| head_projections(o))
                    }
                }
            }
        }
        // a constructor at the head (`%(…)` is the pipe form
        // `($_ | %(…))` without the pipe): its fields
        Operand::Piped { expr, commands } if matches!(**expr, Operand::Topic | Operand::Seed) => {
            for command in commands {
                match command {
                    Command::Func(call) => {
                        for a in &mut call.args {
                            if let Arg::Expr(e) = a {
                                head_projections(e);
                            }
                        }
                    }
                    Command::Expr(e) => head_projections(e),
                    _ => {}
                }
            }
        }
        Operand::Projected { base, .. } => head_projections(base),
        Operand::Index { base, .. } => head_projections(base),
        Operand::Spread(inner) | Operand::Neg(inner) | Operand::Desc(inner) => {
            head_projections(inner)
        }
        Operand::Arith { left, right, .. }
        | Operand::Concat { left, right, .. }
        | Operand::Coalesce { left, right } => {
            head_projections(left);
            head_projections(right);
        }
        _ => {}
    }
}

/// Whether an operand looks at no node: literals and registers
/// alone, lists and arithmetic of them (spec: Index Predicates —
/// such brackets are positional).
pub(crate) fn node_free(op: &Operand) -> bool {
    match op {
        Operand::Lit(_) | Operand::Recall(_) => true,
        Operand::List(items) => items.iter().all(node_free),
        Operand::Arith { left, right, .. } => node_free(left) && node_free(right),
        Operand::Neg(inner) | Operand::Desc(inner) => node_free(inner),
        _ => false,
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
            Command::Func(call) if crate::syntax::registry::known_keyed(&call.name) && !grouped => {
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
                     joined expression: 'A <=> B[::x = $$0::x]'",
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
                        Predicate::Index(_)
                        | Predicate::Range(_, _)
                        | Predicate::Positions(_)
                        | Predicate::Computed(_)
                            if seen_corr =>
                        {
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
            // A leading `:name` (the topic's field on a value, the
            // candidate's core property on a node, draft 157) reads
            // the thread under test, never the outer context: a
            // step predicate `[:name != "refs"]` in a joined
            // expression stays on its step.
            Operand::Field { base, .. } if matches!(**base, Operand::Topic) => {}
            Operand::Invoking(_)
            | Operand::Driver(_)
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
                    | Anchor::MarkBack(_)
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

    /// `defs_only` tells a definitions cell from a query cell with
    /// a definitions preamble, and reports a malformed definition.
    #[test]
    fn defs_only_classifies_cells() {
        let only = "# it's a note\ndef &a: /x ;\nmacro &m: ^ | \"/y\" ;";
        assert_eq!(crate::defs_only(only).unwrap(), true);
        let preamble = "macro &pivot($c): /x | $c @| unique ;\n^ | &pivot(::k) | %.";
        assert_eq!(crate::defs_only(preamble).unwrap(), false);
        let bad = "def &a: /x[ ;";
        assert!(crate::defs_only(bad).is_err());
        assert_eq!(crate::defs_only("# only").unwrap(), true);
    }

    #[test]
    fn fragment_bodies_cover_chains_and_inline_pipes() {
        // A def may name a whole correlation; invoking it stands
        // alone, takes a pipe tail, and extends with further
        // entries.
        for q in [
            "def &j: /a/* <=> /b/*[::x = $$0::x]; &j @| count",
            "def &j: /a/* <=> /b/*[::x = $$0::x]; &j | [!$$1::x]",
            "def &j: /a/* <=> /b/*[::x = $$0::x]; &j <=> /c/*[::y = $$1::y]",
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
                "def &j: /a/* <=> /b/*[::x = $$0::x]; /d/* <=> &j",
                "chains are flat",
            ),
            // a chain fragment cannot join a union
            (
                "def &j: /a/* <=> /b/*[::x = $$0::x]; /d/* || &j",
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
            "/a/* <=> /b/*[::x = $$0::x]",
            "/a/* <=>? /b/*[::x = $$0::x] | [!$$1::x]",
            "/a/* <=> /b/*[::x = $$0::x] <=> /c/*[::y = $$1::y] | %($$1::n; $$2::m)",
            "/a/* <=> /b/*[$$0/kid::x = ::x]",
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
            ("/a/* <=> /b[$$0::x]/c/*", "final step"),
            // Positional selection after an ON clause is ambiguous.
            (
                "/a/* <=> /b/*[::x = $$0::x][1]",
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
        assert!(
            refuse("/x | upper(-i)").contains("'upper' takes no options"),
            "{}",
            refuse("/x | upper(-i)")
        );
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
            crate::syntax::unparse::fn_call_text(&call(vec![
                Arg::Lit(Value::Str("a".into())),
                opt("-n", Some(Operand::Lit(Value::Int(10)))),
                opt("--locale", Some(Operand::Lit(Value::Str("ru".into())))),
                opt("-i", None),
            ])),
            "f(\"a\"; -n=10 --locale=ru -i)"
        );
        assert_eq!(
            crate::syntax::unparse::fn_call_text(&call(vec![opt("-9", None)])),
            "f(-= -9)"
        );
    }

    #[test]
    fn a_prefixed_command_stands_headless_in_a_body() {
        // `%(lemma = morph:lemma)`: a library's or an environment's
        // command applied to the body's seed, like a built-in.
        let toks = lexer::lex("/w | %(lemma = morph:lemma; pos = morph:pos(--lang=ru))").unwrap();
        let q = parse(&toks).unwrap();
        let dbg = format!("{q:?}");
        assert!(
            dbg.contains("name: \"morph:lemma\"") && dbg.contains("name: \"morph:pos\""),
            "{dbg}"
        );
        assert!(dbg.contains("Seed"), "{dbg}");
    }

    #[test]
    fn bare_inf_and_nan_are_float_literals() {
        // `nan`, `inf` and `-inf` are the float literals a printed
        // value reads back as (spec: Numbers, draft 166); quoted,
        // the words are text.
        let toks = lexer::lex("/x[::status = inf]").unwrap();
        let q = parse(&toks).unwrap();
        let dbg = format!("{q:?}");
        assert!(dbg.contains("Lit(Float(inf))"), "got {dbg}");
        let toks = lexer::lex("/x[::status = nan]").unwrap();
        let dbg = format!("{:?}", parse(&toks).unwrap());
        assert!(dbg.contains("Lit(Float(NaN))"), "got {dbg}");
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

/// The bare English words `and` / `or` / `not` were the last undotted
/// keywords; they are retired, and a bare one in operator position
/// is refused with the pointer.
const BARE_BOOL_WORDS: &[(&str, &str)] = &[("and", "&&"), ("or", "||"), ("not", "!")];

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

/// `a = :x<int>` — a typed projector right of `=` is refused (spec:
/// Projectors, draft 166): a trait describes a field, never a value;
/// the pointer names the declared key and the cast.
fn typed_value_refusal(key: &str, inner: &Operand, typing: crate::value::Typing) -> QuarbError {
    let value = crate::syntax::unparse::operand(inner);
    let cast = match typing.declared() {
        Some(t) => format!(", or cast the value, '{key} = {}({value})'", t.word()),
        None => String::new(),
    };
    QuarbError::parse(format!(
        "'{value}{}' right of '=' declares nothing: a trait describes a field, never a value; declare the key, '{key}{} = {value}'{cast}",
        typing.spelling(),
        typing.spelling()
    ))
}

/// A label written bare in the double bracket read as the literal it
/// spells: an ISO date or instant (`2013-01-02`, one word to the
/// lexer, text as an operand) is the instant, and a range of two
/// such literals (`2013-01-02..2013-01-04`) the range operand of its
/// bounds; a quoted label and any other label stay as written.
fn label_range(label: Operand, scale: &dyn Fn(&str) -> Option<(f64, String)>) -> Operand {
    let Operand::Lit(Value::Str(text)) = &label else {
        return label;
    };
    let instant = |t: &str| {
        crate::temporal::parse_iso(t).map(|(secs, nanos, offset_min)| Value::Instant {
            secs,
            nanos,
            offset_min,
            calendar: Default::default(),
        })
    };
    let Some((a, b)) = text.split_once("..") else {
        return match instant(text) {
            Some(v) => Operand::Lit(v),
            None => label,
        };
    };
    if a.is_empty() || b.is_empty() || b.contains("..") {
        return label;
    }
    let bound = |t: &str| match literal_value(t, false, scale) {
        Value::Str(s) => instant(&s).unwrap_or(Value::Str(s)),
        v => v,
    };
    Operand::Range {
        from: Box::new(Operand::Lit(bound(a))),
        to: Box::new(Operand::Lit(bound(b))),
        step: None,
    }
}

/// Whether an operand is a bare projection — `:name` on the topic,
/// `::k` / `:::k` with no steps on the current node — the spelling a
/// typed projector's brackets glue to.
fn is_bare_projection(o: &Operand) -> bool {
    use crate::syntax::ast::{Anchor, Projection};
    match o {
        Operand::Field { base, .. } => matches!(**base, Operand::Topic),
        Operand::Rel {
            steps,
            projection: Some(p),
            anchor: Anchor::Current,
        } => steps.is_empty() && !matches!(p, Projection::Property(None)),
        _ => false,
    }
}

/// Every typed projector written in a structured projector's shape
/// becomes the typed projector value (spec: Projectors, draft 166),
/// through the lists, records and tables that hold it; the bare
/// projections beside it are read on the base as before.
fn fold_typed(op: &mut Operand) {
    use crate::syntax::ast::TableRow;
    match op {
        Operand::Typed { .. } => head_projections(op),
        Operand::List(items) => items.iter_mut().for_each(fold_typed),
        Operand::Table { rows, .. } => {
            for row in rows {
                match row {
                    TableRow::List(items) => items.iter_mut().for_each(fold_typed),
                    TableRow::Record(items) => items.iter_mut().for_each(|(_, o)| fold_typed(o)),
                }
            }
        }
        Operand::Piped { expr, commands } if matches!(**expr, Operand::Topic | Operand::Seed) => {
            for command in commands {
                if let Command::Func(call) = command {
                    for a in &mut call.args {
                        if let Arg::Expr(e) = a {
                            fold_typed(e);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
