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
    Arg, ArithOp, Axis, Branch, CmpOp, FnCall, Group, InterpSeg, Matcher, Operand, PathElem,
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
    };
    p.parse()
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
/// its variant), or a procedural macro — a query evaluated at
/// expansion time against the invocation's expansion arbor, whose
/// text results become the expansion.
#[derive(Debug, Clone)]
enum DefBody {
    Query(Query),
    Pipeline(Vec<Stage>),
    Macro(Query),
}

/// Which pipe introduces a stage — determined by its variant.
fn stage_pipe(stage: &Stage) -> &'static str {
    match stage {
        Stage::Agg(_) | Stage::Select(_) => "@|",
        _ => "|",
    }
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
    /// reverse resolution (`<~`) is refused: it walks the whole arbor
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
}

/// Nesting depth past which parsing refuses (see
/// [`Parser::nest_depth`]). Far beyond any real query, and small
/// enough that the deepest frame chain (each `(` costs several
/// stack frames through the group/operand dual reading) fits the
/// smallest stacks in play — test threads (2 MB) and wasm.
const MAX_NEST: usize = 64;

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
        // A chain of expressions joined by `<=>`; all but the last
        // become the final query's correlation contexts.
        let mut exprs = vec![self.parse_query()?];
        while matches!(self.peek(), Some(Token::Correlate)) {
            self.pos += 1;
            // `<=>?` — the outer marker flags the entry to its
            // LEFT: that context may bind null.
            if matches!(self.peek(), Some(Token::Question)) {
                self.pos += 1;
                if let Some(prev) = exprs.last_mut() {
                    prev.outer = true;
                }
            }
            exprs.push(self.parse_query()?);
        }
        if let Some(tok) = self.peek() {
            return Err(QuarbError::Parse(format!(
                "unexpected trailing input at token {tok:?}"
            )));
        }
        let mut query = exprs.pop().unwrap();
        query.correlations = exprs;
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
        self.union_element(&mut branches, &mut pipeline)?;
        while matches!(self.peek(), Some(Token::PipePipe)) {
            if !pipeline.is_empty() {
                return Err(QuarbError::Parse(
                    "a fragment carrying a pipeline must stand alone, not in a union".into(),
                ));
            }
            self.pos += 1;
            self.union_element(&mut branches, &mut pipeline)?;
        }

        // Pipeline over the whole union.
        self.pipeline_items(&mut pipeline)?;
        Ok(Query {
            correlations: Vec::new(),
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
    ) -> Result<()> {
        if matches!(self.peek(), Some(Token::Amp)) {
            let alone = branches.is_empty();
            let q = self.invoke_query_fragment()?;
            if !q.pipeline.is_empty() && !alone {
                return Err(QuarbError::Parse(
                    "a fragment carrying a pipeline must stand alone, not in a union".into(),
                ));
            }
            branches.extend(q.branches);
            pipeline.extend(q.pipeline);
            if matches!(self.peek(), Some(Token::LBracket)) {
                return Err(QuarbError::Parse(
                    "a fragment does not take trailing predicates; \
                     refine it with a pipeline filter: '&name | [cond]'"
                        .into(),
                ));
            }
            if matches!(
                self.peek(),
                Some(Token::ColonColon | Token::ColonColonColon | Token::SemiSemiSemi)
            ) {
                return Err(QuarbError::Parse(
                    "a fragment does not take a trailing projection; \
                     project it through the pipe: '&name | ::key'"
                        .into(),
                ));
            }
        } else {
            branches.push(self.branch()?);
        }
        Ok(())
    }

    /// Parse pipeline stages onto `pipeline` until something that is
    /// not a pipe. Shared by queries and pipeline-fragment bodies.
    fn pipeline_items(&mut self, pipeline: &mut Vec<Stage>) -> Result<()> {
        loop {
            match self.peek() {
                Some(Token::Pipe) => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(Token::Amp)) {
                        self.invoke_pipeline_fragment("|", pipeline)?;
                        continue;
                    }
                    pipeline.push(self.pipe_item()?);
                }
                // `$| stage` — the map pipe.
                Some(Token::Dollar) if matches!(self.toks.get(self.pos + 1), Some(Token::Pipe)) => {
                    self.pos += 2;
                    pipeline.push(Stage::Map(Box::new(self.map_stage()?)));
                }
                Some(Token::At) => {
                    self.pos += 1;
                    self.expect(Token::Pipe, "'|' after '@' for aggregation")?;
                    if matches!(self.peek(), Some(Token::Amp)) {
                        self.invoke_pipeline_fragment("@|", pipeline)?;
                        continue;
                    }
                    // `@| [n]` / `@| [a..b]` — positional selection
                    // from the whole context.
                    if matches!(self.peek(), Some(Token::LBracket)) {
                        match self.predicate()? {
                            pred @ (Predicate::Index(_) | Predicate::Range(_, _)) => {
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
                    pipeline.push(Stage::Agg(call));
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Parse one pipeline stage after a `|`: a recall, a push, a
    /// subcontext, or a function.
    fn pipe_item(&mut self) -> Result<Stage> {
        match self.peek() {
            // `| $.name`, `| $_`, `| $ord`, and any arithmetic over
            // them are value expressions; a plain recall is just the
            // single-operand case.
            Some(Token::Dollar) => Ok(Stage::Expr(self.additive()?)),
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
                self.expect_dot()?;
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
                                segs.push(InterpSeg::Expr(self.parse_hole(&src)?));
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
            // `| %.` — the named register view, as a record.
            Some(Token::Percent) => {
                self.pos += 1;
                self.expect_dot()?;
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
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.pos += 1;
                    // A subcontext body is a navigating sub-query; a
                    // value expression (`.total(::price * ::qty)`) is
                    // the fallback when the query reading does not
                    // reach the closing parenthesis.
                    let save = self.pos;
                    if let Ok(body) = self.parse_query()
                        && matches!(self.peek(), Some(Token::RParen))
                    {
                        self.pos += 1;
                        return Ok(Stage::Subcontext {
                            name,
                            body: Box::new(body),
                        });
                    }
                    self.pos = save;
                    let expr = self.additive()?;
                    self.expect(Token::RParen, "')' to close a subcontext")?;
                    Ok(Stage::ExprPush { name, expr })
                } else {
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
            // relative path, a parenthesized group, or an
            // interpolated string: `| ::price * ::qty`,
            // `| "${::name} (${::age})"`.
            Some(
                Token::ColonColon
                | Token::ColonColonColon
                | Token::SemiSemiSemi
                | Token::Slash
                | Token::SlashSlash
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

    /// Consume a `Name` that is exactly `.` (for `@.`).
    fn expect_dot(&mut self) -> Result<()> {
        match self.peek() {
            Some(Token::Name { text, .. }) if text == "." => {
                self.pos += 1;
                Ok(())
            }
            _ => Err(QuarbError::Parse("expected '.' after '@'".into())),
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
        self.expect(Token::Colon, "':' between the fragment name and its body")?;

        self.def_params = params.clone();
        // A body starting with a pipe is a pipeline fragment; anything
        // else is a navigation query (which may carry a pipeline).
        let body = if matches!(self.peek(), Some(Token::Pipe | Token::At)) {
            let mut stages = Vec::new();
            self.pipeline_items(&mut stages)?;
            if stages.is_empty() {
                return Err(QuarbError::Parse(format!(
                    "fragment '&{name}' has an empty body"
                )));
            }
            DefBody::Pipeline(stages)
        } else {
            DefBody::Query(self.parse_query()?)
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
        self.expect(Token::Colon, "':' between the macro name and its body")?;
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

    /// Expand a macro invocation to query text: bind arguments
    /// (literals by value, forms by their unparsed text), build the
    /// expansion arbor, run the body against it, and join the text
    /// results.
    fn expand_macro_text(&self, name: &str, def: &Def, args: Vec<Operand>) -> Result<String> {
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
        let text = values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        if text.trim().is_empty() {
            return Err(QuarbError::Parse(format!(
                "macro '&{name}' expanded to nothing"
            )));
        }
        Ok(text)
    }

    /// Parse `&name`, `&name(arg, …)`, or the data-aware `&name!(…)`
    /// and return the name, the argument forms, and whether the `!`
    /// was spelled.
    fn invocation(&mut self) -> Result<(String, Vec<Operand>, bool)> {
        self.expect(Token::Amp, "'&'")?;
        let name = match self.bump() {
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
        let bang = matches!(self.peek(), Some(Token::Bang));
        if bang {
            self.pos += 1;
        }
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            if !matches!(self.peek(), Some(Token::RParen)) {
                loop {
                    args.push(self.additive()?);
                    if matches!(self.peek(), Some(Token::Comma)) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen, "')' to close fragment arguments")?;
        }
        Ok((name, args, bang))
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
    /// query.
    fn invoke_query_fragment(&mut self) -> Result<Query> {
        let (name, args, bang) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::Parse(format!("unknown fragment '&{name}'")));
        };
        self.check_bang(&name, &def, bang)?;
        match def.body {
            DefBody::Query(mut q) => {
                let map = self.bind(&name, &def.params, args)?;
                subst_query(
                    &mut q,
                    &Subst {
                        outer: &map,
                        hole: &map,
                    },
                );
                Ok(q)
            }
            DefBody::Pipeline(_) => Err(QuarbError::Parse(format!(
                "'&{name}' is a pipeline fragment; invoke it after a pipe"
            ))),
            // A macro expands to text, reparsed here as a query.
            DefBody::Macro(_) => {
                let text = self.expand_macro_text(&name, &def, args)?;
                let wrap = |e: QuarbError| {
                    QuarbError::Parse(format!("in expansion of '&{name}' ('{text}'): {e}"))
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                if matches!(tokens.first(), Some(Token::Pipe | Token::At)) {
                    return Err(QuarbError::Parse(format!(
                        "macro '&{name}' expanded to a pipeline fragment \
                         ('{text}'); invoke it after a pipe"
                    )));
                }
                parse_with_data(&tokens, self.defs.before(&name), self.data).map_err(wrap)
            }
        }
    }

    /// Expand a pipeline-fragment invocation onto `pipeline`. The
    /// invoking pipe must match the fragment's first pipe.
    fn invoke_pipeline_fragment(
        &mut self,
        pipe: &'static str,
        pipeline: &mut Vec<Stage>,
    ) -> Result<()> {
        let (name, args, bang) = self.invocation()?;
        let Some(def) = self.defs.get(&name).cloned() else {
            return Err(QuarbError::Parse(format!("unknown fragment '&{name}'")));
        };
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
            // A macro expands to text; here it must be a stage
            // sequence whose first pipe matches the invocation.
            DefBody::Macro(_) => {
                let text = self.expand_macro_text(&name, &def, args)?;
                let wrap = |e: QuarbError| {
                    QuarbError::Parse(format!("in expansion of '&{name}' ('{text}'): {e}"))
                };
                let tokens = lexer::lex(&text).map_err(wrap)?;
                let first = match tokens.first() {
                    Some(Token::Pipe) => "|",
                    Some(Token::At) => "@|",
                    _ => {
                        return Err(QuarbError::Parse(format!(
                            "macro '&{name}' expanded to a query fragment \
                             ('{text}'); invoke it at path position"
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
                };
                let mut stages = Vec::new();
                p.pipeline_items(&mut stages).map_err(wrap)?;
                if p.pos != tokens.len() {
                    return Err(QuarbError::Parse(format!(
                        "macro '&{name}' expanded to text with trailing \
                         content ('{text}')"
                    )));
                }
                pipeline.extend(stages);
                Ok(())
            }
        }
    }

    fn branch(&mut self) -> Result<Branch> {
        // Optional explicit root anchor (the default is already root).
        // A lone `^` is a complete branch: the root itself as the
        // context (`^ | count`, non-navigating macro bodies).
        let anchored = matches!(self.peek(), Some(Token::Caret));
        if anchored {
            self.pos += 1;
        }
        // `(name)` — anchor the branch on a marked node. Only the
        // exact shape LParen bare-name RParen followed by a path
        // continuation reads as an anchor; groups keep `(` for
        // everything else.
        let mark = self.mark_anchor();

        let mut steps = Vec::new();
        while let Some(tok) = self.peek() {
            if matches!(
                tok,
                Token::Pipe
                    | Token::PipePipe
                    | Token::At
                    | Token::RParen
                    | Token::Correlate
                    | Token::Semi
                    | Token::Amp
            ) {
                break;
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

        let projection = self.projection()?;

        if steps.is_empty() && projection.is_none() && !anchored {
            return Err(QuarbError::Parse(
                "a query branch needs at least one step or a projection".into(),
            ));
        }
        Ok(Branch {
            steps,
            projection,
            anchored,
            mark,
        })
    }

    /// Peek-only form of [`Self::mark_anchor`], for match guards.
    fn mark_anchor_ahead(&self) -> bool {
        let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.toks.get(self.pos + 1)
        else {
            return false;
        };
        !text.starts_with('.')
            && matches!(self.toks.get(self.pos + 2), Some(Token::RParen))
            && matches!(
                self.toks.get(self.pos + 3),
                Some(
                    Token::Slash
                        | Token::SlashSlash
                        | Token::Backslash
                        | Token::BackslashBackslash
                        | Token::ArrowOut
                        | Token::ArrowIn
                        | Token::ColonColon
                        | Token::ColonColonColon
                        | Token::SemiSemiSemi
                )
            )
    }

    /// Try the `(name)` mark-anchor lookahead: LParen, one bare
    /// name, RParen, then a token that continues a path (an axis or
    /// a projection). Consumes and returns the name on a hit;
    /// leaves the position untouched otherwise.
    fn mark_anchor(&mut self) -> Option<String> {
        let Some(Token::LParen) = self.toks.get(self.pos) else {
            return None;
        };
        let Some(Token::Name {
            text,
            quoted: false,
            ..
        }) = self.toks.get(self.pos + 1)
        else {
            return None;
        };
        if text.starts_with('.') || text.contains('/') {
            return None;
        }
        if !matches!(self.toks.get(self.pos + 2), Some(Token::RParen)) {
            return None;
        }
        if !matches!(
            self.toks.get(self.pos + 3),
            Some(
                Token::Slash
                    | Token::SlashSlash
                    | Token::Backslash
                    | Token::BackslashBackslash
                    | Token::ArrowOut
                    | Token::ArrowIn
                    | Token::ColonColon
                    | Token::ColonColonColon
                    | Token::SemiSemiSemi
            )
        ) {
            return None;
        }
        let name = text.clone();
        self.pos += 3;
        Some(name)
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
            if !matches!(self.peek(), Some(Token::RParen)) {
                loop {
                    args.push(self.func_arg()?);
                    if matches!(self.peek(), Some(Token::Comma)) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen, "')' to close function arguments")?;
        }
        // A range argument is `window`'s span; nothing else takes one.
        if name != "window" && args.iter().any(|a| matches!(a, Arg::Range(_, _))) {
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
        match self.additive()? {
            Operand::Lit(v) => Ok(Arg::Lit(v)),
            expr => Ok(Arg::Expr(expr)),
        }
    }

    /// Parse an optional trailing projection (`::`, `:::`, `;;;`).
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
                Projection::AdapterMeta(self.require_projection_name("adapter metadata `;;;`")?)
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
            if !quoted && matches!(text.as_str(), "and" | "or" | "not") {
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
            && text.len() > 1
            && !text[1..].starts_with('.')
            && (self.pattern_depth == 0
                || !matches!(self.toks.get(self.pos + 1), Some(Token::LParen)))
        {
            let name = text[1..].to_string();
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
        let body = if let Ok(q) = self.parse_query()
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
                    "expected a property name before '~>' or '<~'".into(),
                ));
            }
        };
        let reverse = matches!(self.bump(), Some(Token::ReverseResolve));
        if reverse && self.predicate_depth > 0 {
            // Reverse resolution scans the whole arbor per candidate
            // node; a predicate's nested paths must stay descending
            // (outgoing `->`/`~>` and incoming `<-` are fine).
            return Err(QuarbError::Parse(
                "reverse resolution '<~' is not allowed inside a predicate \
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
        self.expect(Token::RBracket, "']' to close a predicate")?;
        Ok(Predicate::Expr(expr))
    }

    fn pred_or(&mut self) -> Result<PredExpr> {
        let mut left = self.pred_and()?;
        while self.eat_keyword("or")
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
        while self.eat_keyword("and")
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
        if self.eat_keyword("not") {
            return Ok(PredExpr::Not(Box::new(self.pred_not()?)));
        }
        self.pred_primary()
    }

    fn pred_primary(&mut self) -> Result<PredExpr> {
        let left = self.additive()?;
        if let Some(op) = self.cmp_op() {
            let right = self.additive()?;
            Ok(PredExpr::Compare(left, op, right))
        } else {
            Ok(PredExpr::Truthy(left))
        }
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
                    Some(Token::Pipe) => {
                        self.pos += 1;
                        stages.push(self.inline_stage()?);
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

    /// One `| ...` stage of an inline pipe. The pipeline's own
    /// stage parser, minus what needs a real capsa: pushes and
    /// subcontexts refuse.
    fn inline_stage(&mut self) -> Result<Stage> {
        let stage = self.pipe_item()?;
        match &stage {
            Stage::Push(_) | Stage::ExprPush { .. } | Stage::Subcontext { .. } => {
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
                        | Token::ArrowOut
                        | Token::ArrowIn
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
            anchored: false,
            mark: None,
        })
    }

    /// Parse a comparison operand: a relative path/projection, or a
    /// literal.
    /// Parse one `${...}` hole's source as a value expression, in
    /// the same fragment table and parameter scope.
    fn parse_hole(&mut self, src: &str) -> Result<Operand> {
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
        };
        let expr = p.additive().map_err(context)?;
        if p.pos != tokens.len() {
            return Err(QuarbError::Parse(format!(
                "in '${{{src}}}': an interpolation hole holds one value expression"
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
        let inner = self.operand()?;
        match inner {
            Operand::Recall(_) | Operand::Topic | Operand::Ordinal | Operand::Capture(_) => {}
            Operand::Ctx { .. } => {
                return Err(QuarbError::Parse(
                    "the context-history accessor '$$*' is reserved (unbuilt);                      '$$' steps a capsa-scope operand out one level                      ($$.name, $$_, $$ord)"
                        .into(),
                ));
            }
            _ => {
                return Err(QuarbError::Parse(
                    "'$$' takes a capsa-scope operand ($$.name, $$_, $$ord, $$1)".into(),
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
            // `(name)` — a mark-anchored operand path: navigate
            // from the labeled node. Same lookahead as the branch
            // form; parenthesized expressions keep `(` otherwise.
            Some(Token::LParen) if self.mark_anchor_ahead() => {
                let mark = self.mark_anchor().expect("lookahead hit");
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
                                | Token::ArrowOut
                                | Token::ArrowIn
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
                    anchored: false,
                    mark: Some(mark),
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
                                | Token::ArrowOut
                                | Token::ArrowIn
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
                    anchored: true,
                    mark: None,
                })
            }
            // A relative path operand. It may descend (`/`, `//`) or
            // follow a crosslink (`->`, `<-`), so a structural
            // predicate can ask "has any outgoing link?" with `[->*]`.
            Some(Token::Slash | Token::SlashSlash | Token::ArrowOut | Token::ArrowIn) => {
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
                                | Token::ArrowOut
                                | Token::ArrowIn
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
                    anchored: false,
                    mark: None,
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
                                | Token::ArrowOut
                                | Token::ArrowIn
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
                    anchored: false,
                    mark: None,
                })
            }
            Some(Token::ColonColon | Token::ColonColonColon | Token::SemiSemiSemi) => {
                let projection = self.projection()?.expect("projection start");
                Ok(Operand::Rel {
                    steps: Vec::new(),
                    projection: Some(projection),
                    anchored: false,
                    mark: None,
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
                            let expr = self.parse_hole(&src)?;
                            segs.push(InterpSeg::Expr(expr));
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
            // `@-` — all arrived-by edges (bare: labels; projected:
            // each edge's property); `@.` — the whole register.
            Some(Token::At) => {
                self.pos += 1;
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
                    Some(Token::Slash | Token::SlashSlash | Token::LParen)
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
            other => Err(QuarbError::Parse(format!(
                "expected a value or path in a predicate, found {other:?}"
            ))),
        }
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
            _ => return None,
        };
        self.pos += 1;
        Some(op)
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

    fn trait_or(&mut self) -> Option<TExpr> {
        let mut left = self.trait_and()?;
        while matches!(self.peek(), Some(Token::PipePipe)) {
            self.pos += 1;
            let right = self.trait_and()?;
            left = TExpr::Or(Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn trait_and(&mut self) -> Option<TExpr> {
        let mut left = self.trait_not()?;
        while matches!(self.peek(), Some(Token::AmpAmp)) {
            self.pos += 1;
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
            Some(Token::Gt) => Axis::NextSibling,
            Some(Token::Lt) => Axis::PrevSibling,
            Some(Token::FollowingSiblings(mark)) => Axis::FollowingSiblings(mark_reach(*mark)),
            Some(Token::PrecedingSiblings(mark)) => Axis::PrecedingSiblings(mark_reach(*mark)),
            Some(Token::ArrowOut) => Axis::OutLink,
            Some(Token::ArrowIn) => Axis::InLink,
            Some(Token::Name { text, .. }) => {
                return Err(QuarbError::Parse(format!(
                    "expected a navigation operator before '{text}' \
                     (queries are root-anchored; start with '/')"
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
fn validate_record_convention(call: &FnCall, what: &str) -> Result<()> {
    if call.args.is_empty() {
        return Err(QuarbError::Parse(format!(
            "{what} needs at least one field, e.g. {what}(::name)"
        )));
    }
    let mut i = 0;
    while i < call.args.len() {
        match &call.args[i] {
            // A literal string names the following argument.
            Arg::Lit(Value::Str(_)) => {
                if i + 1 >= call.args.len() {
                    return Err(QuarbError::Parse(format!(
                        "{what} has a trailing field name with no value"
                    )));
                }
                i += 2;
            }
            Arg::Expr(e) if crate::ast::auto_field_name(e).is_some() => i += 1,
            _ => {
                return Err(QuarbError::Parse(format!(
                    "a {what} field needs a name: precede a computed value with a \
                     literal, e.g. {what}(\"total\", ::price * ::qty)"
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
        Stage::Func(call) | Stage::Agg(call) => {
            for arg in &mut call.args {
                if let Arg::Expr(e) = arg {
                    subst_operand(e, map);
                }
            }
        }
        Stage::Expr(e) | Stage::ExprPush { expr: e, .. } => subst_operand(e, map),
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
        // `<~` walks the whole arbor per node, so it is refused inside a
        // predicate — but stays legal in top-level navigation, and the
        // bounded incoming edge `<-` is allowed in predicates.
        assert!(parse(&lexer::lex("//a[::r<~]").unwrap()).is_err());
        assert!(parse(&lexer::lex("//a::r<~").unwrap()).is_ok());
        assert!(parse(&lexer::lex("//a[<-b]").unwrap()).is_ok());
    }
}
