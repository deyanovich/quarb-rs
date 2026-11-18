//! SQL importer for Quarb.
//!
//! Translates a SQL `SELECT` statement into an equivalent Quarb
//! query, following the mapping in the SQL cookbook — in reverse.
//! The translatable subset covers the single-statement query core;
//! everything else is refused with an
//! [`SqlError::Unsupported`] naming the construct, never silently
//! approximated.
//!
//! - `SELECT cols FROM t` → `/t/* | %(::col; …)`; `SELECT *` →
//!   the row nodes; `SELECT DISTINCT col` → `/t/*::col @| unique`,
//!   and over several columns the records, `| %(…) @| unique`.
//! - `WHERE` → a predicate that holds exactly where SQL finds the
//!   condition TRUE. SQL's logic has a third value for a comparison
//!   with a NULL, so `<>` is guarded (`::x != null && ::x != "GB"`)
//!   and a `NOT` is carried inward to the comparisons; `IS [NOT]
//!   NULL` → `= null` / `!= null`; `[NOT] IN (…)` → a chain of
//!   equalities; `[NOT] LIKE` → a regex, `%` a run of characters and
//!   `_` one.
//! - Aggregates (`COUNT(*)`, `SUM`/`AVG`/`MIN`/`MAX`) → `@|`
//!   reductions over the piped column, `COUNT(col)` →
//!   `count(--values)`; `GROUP BY k` → `| .k(::k) @|{.k}` with the
//!   aggregate folding by the pushed key and `HAVING` a filter
//!   after it.
//! - `JOIN t2 ON …` → the every-witness correlation, one thread for
//!   every pair as SQL has one row: `/t1/* <=>* /t2/*[::b =
//!   $$0::a]`, the FROM table driving, the ON in the joined
//!   expression's bracket, the joined side's columns read through
//!   the witness (`$$1::col`). `LEFT JOIN` → `<=>*?`, which keeps a
//!   driver with no pair, its WHERE a filter after the join; `RIGHT
//!   JOIN` is `LEFT` with the tables' places exchanged; `CROSS JOIN`
//!   is every pair. The ON is any condition.
//! - `UNION ALL` of single columns of different tables → a branch
//!   union, `/t/*::a || /u/*::b`; `UNION` the same and `@| unique`.
//! - `ORDER BY` → `@| sort_by(…)` (`DESC` via `@| reverse`);
//!   `LIMIT n` → `@| [..n]`.
//!
//! Refused: subqueries, window functions, `CASE`, a `FULL` join,
//! multi-join chains (one `JOIN` translates), `SELECT *` over a
//! join, `UNION ALL` over one table twice (a branch union keeps one
//! thread for a node two branches reach), a comparison with a NULL
//! literal, `HAVING` without `GROUP BY`, aggregate arithmetic
//! (`SUM(a) + 1`), and any non-`SELECT` statement.
//!
//! Each translation is held against SQLite's own answer over one
//! database (the differential rig of `tests/sql.rs`). What differs
//! between SQL engines themselves is reported as a
//! [`Translation::notes`] line rather than refused:
//!
//! - Quarb streams records (JSONL), not a result table.
//! - `LIKE` translates to a case-insensitive regex (matching
//!   MySQL's and SQLite's default folding; PostgreSQL's `LIKE`
//!   is case-sensitive).
//! - `ORDER BY` puts a NULL key last, first under `DESC`, as
//!   PostgreSQL does; SQLite and MySQL put it first.

mod export;
pub use export::{
    ColumnFact, ColumnTypes, Dialect, FloatSum, Partial, Pushdown, export, partial_pushdown,
    partial_pushdown_explained, partial_pushdown_keyed, pushdown, pushdown_explained,
};

use quarb::diagnostic as d;
use std::fmt::Write as _;

/// An error translating a SQL statement.
#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error("SQL syntax error: {0}")]
    Syntax(String),
    #[error("unsupported SQL construct: {0}")]
    Unsupported(String),
    /// A construct of SQL with no Quarb translation: the diagnostic
    /// code of the reason (`JOIN_NOT_TRANSLATED`; ruling 2026-11-17,
    /// one word per reason) and the reason.
    #[error("unsupported SQL construct: {1}")]
    NotTranslated(&'static str, String),
    /// SQL can spell the construct, but with provably different
    /// semantics (NULL propagation, collation, coercion) — the
    /// refusal names the divergence, not a missing feature.
    #[error("semantic mismatch: {0}")]
    Semantics(String),
    /// A part of a Quarb query the exporter leaves to the engine:
    /// the word a planner's refusal is told under, whether SQL
    /// could spell the part with other semantics (`mismatch`) or
    /// has no form for it, and the reason.
    #[error("{}: {reason}", class(*.mismatch))]
    NotPushed {
        word: Refusal,
        mismatch: bool,
        reason: String,
    },
}

/// The class an exporter's refusal prints before its reason.
fn class(mismatch: bool) -> &'static str {
    if mismatch {
        "semantic mismatch"
    } else {
        "unsupported SQL construct"
    }
}

impl SqlError {
    /// The word and the reason of an exporter's refusal; `None` for
    /// a syntax error and for an importer's error.
    pub fn not_pushed(&self) -> Option<(Refusal, &str)> {
        match self {
            SqlError::NotPushed { word, reason, .. } => Some((*word, reason)),
            _ => None,
        }
    }

    /// The code and the reason of an importer's refusal, and
    /// whether `--lenient` translates what was refused: never, the
    /// SQL bridge has no lenient form.
    pub fn not_translated(&self) -> Option<(&'static str, &str, bool)> {
        match self {
            SqlError::NotTranslated(code, reason) => Some((code, reason, false)),
            _ => None,
        }
    }
}

/// Why the exporter leaves a part of a Quarb query to the engine:
/// the first word of the diagnostic code a planner's refusal is told
/// under (`Sort` is `SORT_NOT_PUSHED`; ruling 2026-11-17, one word
/// per reason where `NOT_PUSHED` was the one kind). The word names
/// what the user wrote; the inventory (`langspec/diagnostics.tsv`)
/// files each under its remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// A sort (`sort_by`, `top`, `reverse`): a database orders text
    /// by its own collation.
    Sort,
    /// A fold, a command over its groups, a HAVING.
    Fold,
    /// `unique`: DISTINCT returns rows in no stated order.
    Unique,
    /// A selection by position.
    Take,
    /// A regular expression, a substring test, a LIKE.
    Pattern,
    /// A condition that follows a reference or navigates.
    Resolution,
    /// Navigation beyond a table's rows.
    Walk,
    /// A read of the query's own state: a register, the topic, a
    /// capture, a mark, a pushed value.
    State,
    /// A correlation in a form with no JOIN.
    Join,
    /// A reducer over row nodes, or one SQL has no aggregate for.
    Reducer,
    /// A comparison that would follow the column's declared type
    /// where the engine follows each value.
    Type,
    /// A union of branches.
    Union,
    /// An operator, a command or a projection with no SQL form.
    Operator,
    /// A name or a literal with no spelling every dialect reads, or
    /// an empty query.
    Name,
    /// A leading condition a partial plan could not send ahead:
    /// what follows would see the difference.
    Condition,
    /// A partial plan with no leading condition to send ahead:
    /// nothing was refused, there was nothing to push, and no
    /// diagnostic tells of it.
    Nothing,
    /// A whole plan for a query that ends on row nodes: their
    /// locators are the scan's to write. Nothing the reader wrote
    /// is refused (the conditions go ahead through the partial
    /// plan), and no diagnostic tells of it.
    Nodes,
}

impl Refusal {
    /// The diagnostic code; `None` for [`Refusal::Nothing`] and
    /// [`Refusal::Nodes`].
    pub fn code(self) -> Option<&'static str> {
        use quarb::diagnostic as d;
        Some(match self {
            Refusal::Sort => d::SORT_NOT_PUSHED,
            Refusal::Fold => d::FOLD_NOT_PUSHED,
            Refusal::Unique => d::UNIQUE_NOT_PUSHED,
            Refusal::Take => d::TAKE_NOT_PUSHED,
            Refusal::Pattern => d::PATTERN_NOT_PUSHED,
            Refusal::Resolution => d::RESOLUTION_NOT_PUSHED,
            Refusal::Walk => d::WALK_NOT_PUSHED,
            Refusal::State => d::STATE_NOT_PUSHED,
            Refusal::Join => d::JOIN_NOT_PUSHED,
            Refusal::Reducer => d::REDUCER_NOT_PUSHED,
            Refusal::Type => d::TYPE_NOT_PUSHED,
            Refusal::Union => d::UNION_NOT_PUSHED,
            Refusal::Operator => d::OPERATOR_NOT_PUSHED,
            Refusal::Name => d::NAME_NOT_PUSHED,
            Refusal::Condition => d::CONDITION_NOT_PUSHED,
            Refusal::Nothing | Refusal::Nodes => return None,
        })
    }
}

/// A successful translation: the Quarb query, plus notes on known
/// semantic divergences that apply to it.
#[derive(Debug)]
pub struct Translation {
    pub query: String,
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A bare identifier or keyword (uppercased for keywords).
    Word(String),
    /// A 'string literal'.
    Str(String),
    Num(String),
    Sym(char),
    /// `<=`, `>=`, `<>`, `!=`
    Op2(String),
}

fn lex(input: &str) -> Result<Vec<Tok>, SqlError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    Some('\'') if chars.get(i + 1) == Some(&'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(&ch) => {
                        s.push(ch);
                        i += 1;
                    }
                    None => return Err(SqlError::Syntax("unterminated string".into())),
                }
            }
            out.push(Tok::Str(s));
            continue;
        }
        if c.is_ascii_digit() {
            let mut s = String::new();
            let mut dotted = false;
            while let Some(&ch) = chars.get(i) {
                if ch.is_ascii_digit() || (ch == '.' && !dotted) {
                    dotted |= ch == '.';
                    s.push(ch);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(Tok::Num(s));
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '"' || c == '`' {
            // Quoted identifiers keep their case; bare words are
            // identifiers-or-keywords (keywords match uppercased).
            if c == '"' || c == '`' {
                let quote = c;
                let mut s = String::new();
                let mut closed = false;
                i += 1;
                while let Some(&ch) = chars.get(i) {
                    i += 1;
                    if ch == quote {
                        closed = true;
                        break;
                    }
                    s.push(ch);
                }
                if !closed {
                    return Err(SqlError::Syntax("unterminated quoted identifier".into()));
                }
                out.push(Tok::Word(s));
                continue;
            }
            let mut s = String::new();
            while let Some(&ch) = chars.get(i) {
                if ch.is_alphanumeric() || ch == '_' {
                    s.push(ch);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(Tok::Word(s));
            continue;
        }
        if (c == '<' && matches!(chars.get(i + 1), Some('=') | Some('>')))
            || (c == '>' && chars.get(i + 1) == Some(&'='))
            || (c == '!' && chars.get(i + 1) == Some(&'='))
        {
            out.push(Tok::Op2(format!("{c}{}", chars[i + 1])));
            i += 2;
            continue;
        }
        if "(),.*=<>;-".contains(c) {
            out.push(Tok::Sym(c));
            i += 1;
            continue;
        }
        return Err(SqlError::Syntax(format!("unexpected character '{c}'")));
    }
    Ok(out)
}

// ---------------------------------------------------------------
// Parser / translator
// ---------------------------------------------------------------

/// A column reference: optionally table-qualified.
#[derive(Debug, Clone)]
struct ColRef {
    table: Option<String>,
    column: String,
}

#[derive(Debug, Clone)]
enum Scalar {
    Col(ColRef),
    Str(String),
    Num(String),
    Null,
}

#[derive(Debug)]
enum Cond {
    Cmp(ColRef, String, Scalar),
    /// An aggregate call compared to a scalar — HAVING only.
    AggCmp(Agg, Option<ColRef>, String, Scalar),
    Like(ColRef, String),
    IsNull(ColRef, bool),
    In(ColRef, Vec<Scalar>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Not(Box<Cond>),
}

#[derive(Debug, Clone, PartialEq)]
enum Agg {
    Count,
    CountCol,
    Sum,
    Avg,
    Min,
    Max,
}

/// The aggregate a keyword names, if any.
fn agg_kw(w: &str) -> Option<Agg> {
    match w.to_ascii_uppercase().as_str() {
        "COUNT" => Some(Agg::Count),
        "SUM" => Some(Agg::Sum),
        "AVG" => Some(Agg::Avg),
        "MIN" => Some(Agg::Min),
        "MAX" => Some(Agg::Max),
        _ => None,
    }
}

#[derive(Debug)]
enum SelectItem {
    Star,
    Col(ColRef, Option<String>),
    Agg(Agg, Option<ColRef>, Option<String>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn kw(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(word)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, word: &str) -> Result<(), SqlError> {
        if self.kw(word) {
            Ok(())
        } else {
            Err(SqlError::Syntax(format!("expected {word}")))
        }
    }

    fn sym(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Sym(c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Result<String, SqlError> {
        match self.peek() {
            Some(Tok::Word(w)) => {
                let w = w.clone();
                self.pos += 1;
                Ok(w)
            }
            other => Err(SqlError::Syntax(format!(
                "expected an identifier, found {other:?}"
            ))),
        }
    }

    fn col_ref(&mut self) -> Result<ColRef, SqlError> {
        let first = self.ident()?;
        if self.sym('.') {
            let column = self.ident()?;
            Ok(ColRef {
                table: Some(first),
                column,
            })
        } else {
            Ok(ColRef {
                table: None,
                column: first,
            })
        }
    }

    fn scalar(&mut self) -> Result<Scalar, SqlError> {
        match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.pos += 1;
                Ok(Scalar::Str(s))
            }
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(Scalar::Num(n))
            }
            Some(Tok::Sym('-')) => {
                self.pos += 1;
                match self.peek().cloned() {
                    Some(Tok::Num(n)) => {
                        self.pos += 1;
                        Ok(Scalar::Num(format!("-{n}")))
                    }
                    _ => Err(SqlError::Syntax("expected a number after '-'".into())),
                }
            }
            Some(Tok::Word(w)) if w.eq_ignore_ascii_case("NULL") => {
                self.pos += 1;
                Ok(Scalar::Null)
            }
            Some(Tok::Word(_)) => Ok(Scalar::Col(self.col_ref()?)),
            other => Err(SqlError::Syntax(format!(
                "expected a value, found {other:?}"
            ))),
        }
    }

    // cond := and_cond (OR and_cond)*
    fn cond(&mut self) -> Result<Cond, SqlError> {
        let mut left = self.and_cond()?;
        while self.kw("OR") {
            let right = self.and_cond()?;
            left = Cond::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_cond(&mut self) -> Result<Cond, SqlError> {
        let mut left = self.not_cond()?;
        while self.kw("AND") {
            let right = self.not_cond()?;
            left = Cond::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn not_cond(&mut self) -> Result<Cond, SqlError> {
        if self.kw("NOT") {
            return Ok(Cond::Not(Box::new(self.not_cond()?)));
        }
        if self.sym('(') {
            let inner = self.cond()?;
            if !self.sym(')') {
                return Err(SqlError::Syntax("expected ')'".into()));
            }
            return Ok(inner);
        }
        self.comparison()
    }

    /// An aggregate call's argument list: the parser is past the
    /// name, at `(`. Returns the aggregate (COUNT refined to
    /// CountCol) and its column, `None` for `(*)`.
    fn agg_call(&mut self, mut a: Agg) -> Result<(Agg, Option<ColRef>), SqlError> {
        self.pos += 1; // '('
        let col = if self.sym('*') {
            None
        } else {
            let c = self.col_ref()?;
            if a == Agg::Count {
                a = Agg::CountCol;
            }
            Some(c)
        };
        if !self.sym(')') {
            return Err(SqlError::Syntax("expected ')' after aggregate".into()));
        }
        Ok((a, col))
    }

    fn cmp_op(&mut self) -> Result<String, SqlError> {
        Ok(match self.peek().cloned() {
            Some(Tok::Sym('=')) => {
                self.pos += 1;
                "=".to_string()
            }
            Some(Tok::Sym('<')) => {
                self.pos += 1;
                "<".to_string()
            }
            Some(Tok::Sym('>')) => {
                self.pos += 1;
                ">".to_string()
            }
            Some(Tok::Op2(o)) => {
                self.pos += 1;
                match o.as_str() {
                    "<>" | "!=" => "!=".to_string(),
                    other => other.to_string(),
                }
            }
            other => {
                return Err(SqlError::Syntax(format!(
                    "expected an operator, found {other:?}"
                )));
            }
        })
    }

    fn comparison(&mut self) -> Result<Cond, SqlError> {
        // An aggregate call (`SUM(x) > 5`): meaningful in HAVING,
        // refused with its own message in WHERE.
        if let Some(Tok::Word(w)) = self.peek().cloned()
            && let Some(a) = agg_kw(&w)
            && matches!(self.toks.get(self.pos + 1), Some(Tok::Sym('(')))
        {
            self.pos += 1; // the name
            let (a, col) = self.agg_call(a)?;
            let op = self.cmp_op()?;
            let rhs = self.scalar()?;
            return Ok(Cond::AggCmp(a, col, op, rhs));
        }
        let col = self.col_ref()?;
        if self.kw("IS") {
            let not = self.kw("NOT");
            self.expect_kw("NULL")?;
            return Ok(Cond::IsNull(col, !not));
        }
        // `col NOT LIKE …`, `col NOT IN (…)`: the form, negated
        let word_is = |t: Option<&Tok>, words: &[&str]| matches!(t, Some(Tok::Word(w)) if words.iter().any(|x| w.eq_ignore_ascii_case(x)));
        let negated =
            word_is(self.peek(), &["NOT"]) && word_is(self.toks.get(self.pos + 1), &["LIKE", "IN"]);
        if negated {
            self.pos += 1;
        }
        let said = |c: Cond| {
            if negated { Cond::Not(Box::new(c)) } else { c }
        };
        if self.kw("LIKE") {
            match self.peek().cloned() {
                Some(Tok::Str(p)) => {
                    self.pos += 1;
                    return Ok(said(Cond::Like(col, p)));
                }
                _ => return Err(SqlError::Syntax("LIKE takes a string pattern".into())),
            }
        }
        if self.kw("IN") {
            if !self.sym('(') {
                return Err(SqlError::Syntax("IN takes a parenthesized list".into()));
            }
            let mut items = Vec::new();
            loop {
                items.push(self.scalar()?);
                if !self.sym(',') {
                    break;
                }
            }
            if !self.sym(')') {
                return Err(SqlError::Syntax("expected ')' after IN list".into()));
            }
            return Ok(said(Cond::In(col, items)));
        }
        let op = self.cmp_op()?;
        let rhs = self.scalar()?;
        Ok(Cond::Cmp(col, op, rhs))
    }
}

// ---------------------------------------------------------------
// Emission
// ---------------------------------------------------------------

/// How a JOIN treats a row with no pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinKind {
    /// Every pair that satisfies the ON.
    Inner,
    /// The same, and the driver's rows that have no pair.
    Left,
    /// LEFT with the tables' places exchanged; resolved to `Left`
    /// before anything is emitted.
    Right,
    /// Every pair.
    Cross,
}

/// The tables in scope: (name, alias). The first is the FROM table
/// — the driver under driver-first correlation; the second the
/// JOIN table, whose witness the pipeline reads as `$$1`.
struct Scope {
    from: (String, Option<String>),
    join: Option<(String, Option<String>)>,
    /// Whether operands are being emitted inside the joined
    /// expression's ON bracket (driver = `$$0::col`, joined side
    /// bare) rather than in pipeline position (driver bare,
    /// joined side = `$$1::col`).
    on_clause: bool,
}

impl Scope {
    /// Whether `col` belongs to the FROM (left) table: qualified by
    /// its name or alias, or unqualified with no join.
    fn is_left(&self, col: &ColRef) -> Result<bool, SqlError> {
        let Some(q) = &col.table else {
            if self.join.is_some() {
                return Err(SqlError::NotTranslated(
                    d::JOIN_NOT_TRANSLATED,
                    format!("unqualified column '{}' in a JOIN (qualify it)", col.column),
                ));
            }
            return Ok(true);
        };
        let matches = |(name, alias): &(String, Option<String>)| {
            q.eq_ignore_ascii_case(name)
                || alias.as_ref().is_some_and(|a| q.eq_ignore_ascii_case(a))
        };
        if matches(&self.from) {
            Ok(true)
        } else if self.join.as_ref().is_some_and(matches) {
            Ok(false)
        } else {
            Err(SqlError::Syntax(format!("unknown table qualifier '{q}'")))
        }
    }

    /// The operand for `col`. In pipeline position the driver's
    /// columns are bare and the joined table's ride its witness
    /// (`$$1::col`); inside the ON bracket the joined table's are
    /// bare and the driver is reached as `$$0::col`.
    fn operand(&self, col: &ColRef) -> Result<String, SqlError> {
        let key = quarb_key(&col.column)?;
        Ok(match (self.join.is_some(), self.on_clause) {
            (false, _) => format!("::{key}"),
            (true, false) if self.is_left(col)? => format!("::{key}"),
            (true, false) => format!("$$1::{key}"),
            (true, true) if self.is_left(col)? => format!("$$0::{key}"),
            (true, true) => format!("::{key}"),
        })
    }

    /// Whether an operand, inside the ON bracket, reads the driver
    /// (`$$0::col`): a comparison with it is a key comparison of the
    /// join, which the engine leaves unknown for a null on either
    /// side.
    fn keyed(&self, operand: &str) -> bool {
        self.on_clause && operand.starts_with("$$")
    }

    /// Whether `c` tests a column of the FROM table for NULL: the one
    /// condition a join's bracket cannot hold.
    fn tests_driver_null(&self, c: &Cond) -> bool {
        match c {
            Cond::IsNull(col, _) => self.is_left(col).unwrap_or(false),
            Cond::And(a, b) | Cond::Or(a, b) => {
                self.tests_driver_null(a) || self.tests_driver_null(b)
            }
            Cond::Not(a) => self.tests_driver_null(a),
            _ => false,
        }
    }

    /// This scope, switched to ON-bracket emission.
    fn on(&self) -> Scope {
        Scope {
            from: self.from.clone(),
            join: self.join.clone(),
            on_clause: true,
        }
    }
}

/// A SQL string as a Quarb string literal: double-quoted, with the
/// Quarb lexer's `\"`, `\\`, `\$`, and `` \` `` escapes applied so
/// the content survives verbatim (`$` would otherwise read as an
/// interpolation).
fn quarb_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A SQL identifier as a Quarb name (a step matcher or projection
/// key): bare when it lexes as one, single-quoted otherwise (a
/// quoted SQL identifier can hold spaces or keywords). A name with
/// a quote in it does not translate.
fn quarb_key(name: &str) -> Result<String, SqlError> {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        && !name.starts_with('.')
        && !matches!(name, "and" | "or" | "not");
    if bare {
        return Ok(name.to_string());
    }
    if name.contains('\'') {
        return Err(SqlError::NotTranslated(
            d::NAME_NOT_TRANSLATED,
            format!("the identifier {name:?} (a quote inside a quoted identifier)"),
        ));
    }
    Ok(format!("'{name}'"))
}

/// A scalar as a Quarb operand; columns resolve through the scope.
fn scalar_text(s: &Scalar, scope: &Scope) -> Result<String, SqlError> {
    Ok(match s {
        Scalar::Col(c) => scope.operand(c)?,
        Scalar::Str(v) => quarb_str(v),
        Scalar::Num(n) => n.clone(),
        Scalar::Null => "null".to_string(),
    })
}

/// The operator that holds exactly where `op` fails, both sides
/// present.
fn complement(op: &str) -> &'static str {
    match op {
        "=" => "!=",
        "!=" => "=",
        "<" => ">=",
        "<=" => ">",
        ">" => "<=",
        _ => "<",
    }
}

/// A LIKE pattern as a regex literal. The simple forms keep their
/// short spelling (`%x%` is a search, `x%` and `%x` anchor one end);
/// any other pattern is anchored at both ends, `%` any run of
/// characters and `_` any one, a line break among them.
fn like_regex(pat: &str) -> String {
    let inner = pat.trim_matches('%');
    if !inner.contains('%') && !inner.contains('_') {
        let esc = regex_escape(inner);
        return match (pat.starts_with('%'), pat.ends_with('%')) {
            (true, true) => format!("(/(?i){esc}/)"),
            (false, true) => format!("(/(?i)^{esc}/)"),
            (true, false) => format!("(/(?i){esc}$/)"),
            (false, false) => format!("(/(?i)^{esc}$/)"),
        };
    }
    let mut body = String::new();
    for c in pat.chars() {
        match c {
            '%' => body.push_str(".*"),
            '_' => body.push('.'),
            c => body.push_str(&regex_escape(&c.to_string())),
        }
    }
    format!("(/(?is)^{body}$/)")
}

/// A condition as a Quarb predicate that holds exactly where SQL
/// finds it TRUE (`truth`), or exactly where SQL finds it FALSE.
///
/// SQL's logic has a third value, UNKNOWN, for a comparison with a
/// NULL, and a row passes WHERE only on TRUE. Quarb's is two-valued:
/// a NULL differs from 'GB' (`!=` holds), two NULLs are equal, and
/// the negation of a comparison that failed holds. So a negation is
/// carried inward to the comparisons (`NOT (a AND b)` is where `a`
/// is FALSE or `b` is), and a comparison that would hold for a NULL
/// is guarded: `x <> 'GB'` is `::x != null && ::x != "GB"`. The
/// ordering comparisons need no guard: none holds for a NULL.
///
/// Inside a join's bracket the engine reads a comparison with the
/// driver as SQL does, unknown for a null on either side (spec:
/// Correlation), so such a comparison takes no guard there; and a
/// test of the driver's column for NULL cannot stand there at all
/// ([`Scope::keyed`]).
fn emit_cond(
    c: &Cond,
    truth: bool,
    scope: &Scope,
    notes: &mut Vec<String>,
) -> Result<String, SqlError> {
    Ok(match c {
        Cond::Cmp(col, op, rhs) => {
            if matches!(rhs, Scalar::Null) {
                return Err(SqlError::NotTranslated(
                    d::NULL_NOT_TRANSLATED,
                    format!(
                        "a comparison with NULL ('{} {op} NULL' is never true in SQL: write IS \
                     NULL or IS NOT NULL)",
                        col.column
                    ),
                ));
            }
            let op = if truth { op.as_str() } else { complement(op) };
            let lhs = scope.operand(col)?;
            let r = scalar_text(rhs, scope)?;
            match (op, matches!(rhs, Scalar::Col(_))) {
                // a key comparison of a join condition is unknown
                // for a null on either side, as SQL's is
                _ if scope.keyed(&lhs) || scope.keyed(&r) => format!("{lhs} {op} {r}"),
                ("!=", false) => format!("{lhs} != null && {lhs} != {r}"),
                ("!=", true) => format!("{lhs} != null && {r} != null && {lhs} != {r}"),
                ("=", true) => format!("{lhs} != null && {lhs} = {r}"),
                _ => format!("{lhs} {op} {r}"),
            }
        }
        Cond::AggCmp(..) => {
            return Err(SqlError::NotTranslated(
                d::AGGREGATE_NOT_TRANSLATED,
                "an aggregate in WHERE (SQL puts it in HAVING)".into(),
            ));
        }
        Cond::Like(col, pat) => {
            let lhs = scope.operand(col)?;
            let note = "LIKE: translated to a case-insensitive regex (SQL's default \
                        ASCII case folding)";
            if !notes.iter().any(|n| n == note) {
                notes.push(note.to_string());
            }
            let regex = like_regex(pat);
            if truth {
                format!("{lhs} == {regex}")
            } else if scope.keyed(&lhs) {
                format!("{lhs} != {regex}")
            } else {
                format!("{lhs} != null && !({lhs} == {regex})")
            }
        }
        Cond::IsNull(col, is_null) => {
            // `= null` / `!= null`: Quarb's `value_eq` treats NULL
            // as an ordinary value, so these are exactly SQL's
            // `IS [NOT] NULL` — truthiness (`not ::c`) would also
            // drop 0 and ''.
            let lhs = scope.operand(col)?;
            if scope.keyed(&lhs) {
                return Err(SqlError::NotTranslated(
                    d::JOIN_NOT_TRANSLATED,
                    format!(
                        "IS NULL on '{}', the driving table's column, inside ON (a join condition \
                     leaves a null key unknown: state it in WHERE)",
                        col.column
                    ),
                ));
            }
            if *is_null == truth {
                format!("{lhs} = null")
            } else {
                format!("{lhs} != null")
            }
        }
        Cond::In(col, items) => {
            let lhs = scope.operand(col)?;
            let has_null = items.iter().any(|i| matches!(i, Scalar::Null));
            let listed: Vec<&Scalar> = items
                .iter()
                .filter(|i| !matches!(i, Scalar::Null))
                .collect();
            if truth {
                // a NULL in the list matches nothing
                if listed.is_empty() {
                    return Err(SqlError::NotTranslated(
                        d::NULL_NOT_TRANSLATED,
                        "IN over a list of NULL alone (never true in SQL)".into(),
                    ));
                }
                let parts: Vec<String> = listed
                    .iter()
                    .map(|s| {
                        let r = scalar_text(s, scope)?;
                        let guarded =
                            matches!(s, Scalar::Col(_)) && !scope.keyed(&lhs) && !scope.keyed(&r);
                        Ok(if guarded {
                            format!("{lhs} != null && {lhs} = {r}")
                        } else {
                            format!("{lhs} = {r}")
                        })
                    })
                    .collect::<Result<_, SqlError>>()?;
                format!("({})", parts.join(" || "))
            } else {
                if has_null {
                    return Err(SqlError::NotTranslated(
                        d::NULL_NOT_TRANSLATED,
                        "NOT IN over a list with NULL (never true in SQL)".into(),
                    ));
                }
                let mut parts = Vec::new();
                if !scope.keyed(&lhs) {
                    parts.push(format!("{lhs} != null"));
                }
                for s in listed {
                    let r = scalar_text(s, scope)?;
                    if matches!(s, Scalar::Col(_)) && !scope.keyed(&lhs) && !scope.keyed(&r) {
                        parts.push(format!("{r} != null"));
                    }
                    parts.push(format!("{lhs} != {r}"));
                }
                parts.join(" && ")
            }
        }
        // AND is TRUE where both are, FALSE where either is; OR the
        // other way round. `&&` binds tighter than `||`, and a
        // disjunction carries its own parentheses.
        Cond::And(a, b) | Cond::Or(a, b) => {
            let (a, b) = (
                emit_cond(a, truth, scope, notes)?,
                emit_cond(b, truth, scope, notes)?,
            );
            if matches!(c, Cond::And(..)) == truth {
                format!("{a} && {b}")
            } else {
                format!("({a} || {b})")
            }
        }
        Cond::Not(a) => emit_cond(a, !truth, scope, notes)?,
    })
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "\\.+*?()|[]{}^$/".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

/// The name an aggregate's field takes where the statement gives
/// it no alias: the reducer's word.
fn agg_word(a: &Agg) -> &'static str {
    match a {
        Agg::Count | Agg::CountCol => "count",
        other => agg_fn(other),
    }
}

/// The reducer an aggregate is, as the query writes it.
fn agg_fn(a: &Agg) -> &'static str {
    match a {
        Agg::Count => "count",
        Agg::CountCol => "count(--values)",
        Agg::Sum => "sum",
        Agg::Avg => "mean",
        Agg::Min => "min",
        Agg::Max => "max",
    }
}

/// Translate one SQL `SELECT` statement to a Quarb query.
pub fn translate(sql: &str) -> Result<Translation, SqlError> {
    let toks = lex(sql.trim().trim_end_matches(';'))?;
    // A UNION outside every parenthesis joins whole SELECTs.
    let mut depth = 0usize;
    let mut cuts = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Sym('(') => depth += 1,
            Tok::Sym(')') => depth = depth.saturating_sub(1),
            Tok::Word(w) if depth == 0 && w.eq_ignore_ascii_case("UNION") => cuts.push(i),
            _ => {}
        }
    }
    if !cuts.is_empty() {
        return translate_union(&toks, &cuts);
    }
    translate_select(toks)
}

/// `SELECT col FROM t [WHERE …] UNION [ALL] SELECT col FROM u …`,
/// with an ORDER BY and a LIMIT of the whole after the last: a
/// branch union of the columns, `/t/*[…]::col || /u/*[…]::col`.
///
/// UNION removes duplicate values, and `@| unique` after the union
/// does. UNION ALL keeps every row, and the branch union keeps one
/// thread for a node two branches both reach: over two different
/// tables no node is shared and the translation is exact; over one
/// table twice a row both SELECTs take would answer once where SQL
/// answers twice, so that form is refused.
fn translate_union(toks: &[Tok], cuts: &[usize]) -> Result<Translation, SqlError> {
    let mut notes = Vec::new();
    let mut all = None;
    let mut parts: Vec<&[Tok]> = Vec::new();
    let mut from = 0;
    for &cut in cuts {
        parts.push(&toks[from..cut]);
        let is_all =
            matches!(toks.get(cut + 1), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("ALL"));
        if all.is_some_and(|a| a != is_all) {
            return Err(SqlError::NotTranslated(
                d::UNION_NOT_TRANSLATED,
                "UNION and UNION ALL in one statement".into(),
            ));
        }
        all = Some(is_all);
        from = cut + 1 + usize::from(is_all);
    }
    parts.push(&toks[from..]);
    let all = all.unwrap_or(false);

    let mut branches = Vec::new();
    let mut tables: Vec<String> = Vec::new();
    let mut first_name = String::new();
    let mut tail = String::new();
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        let mut p = Parser {
            toks: part.to_vec(),
            pos: 0,
        };
        let shape = || {
            SqlError::NotTranslated(
                d::UNION_NOT_TRANSLATED,
                "a UNION of anything but single columns ('SELECT col FROM t [WHERE …]' on \
                 each side)"
                    .into(),
            )
        };
        p.expect_kw("SELECT").map_err(|_| shape())?;
        if p.kw("DISTINCT") || p.sym('*') {
            return Err(shape());
        }
        if matches!(p.peek(), Some(Tok::Word(w)) if agg_kw(w).is_some())
            && matches!(p.toks.get(p.pos + 1), Some(Tok::Sym('(')))
        {
            return Err(shape());
        }
        let col = p.col_ref()?;
        let alias = p.kw("AS").then(|| p.ident()).transpose()?;
        if p.sym(',') {
            return Err(shape());
        }
        p.expect_kw("FROM")?;
        let table = p.ident()?;
        let table_alias = if p.kw("AS") {
            Some(p.ident()?)
        } else {
            match p.peek() {
                Some(Tok::Word(w))
                    if !["WHERE", "ORDER", "LIMIT"].contains(&w.to_ascii_uppercase().as_str()) =>
                {
                    Some(p.ident()?)
                }
                _ => None,
            }
        };
        let scope = Scope {
            from: (table.clone(), table_alias),
            join: None,
            on_clause: false,
        };
        let cond = p.kw("WHERE").then(|| p.cond()).transpose()?;
        if i == 0 {
            first_name = alias.unwrap_or_else(|| col.column.clone());
        }
        if i == last {
            // the whole union's order and limit
            if p.kw("ORDER") {
                p.expect_kw("BY")?;
                let named = match p.peek().cloned() {
                    Some(Tok::Num(n)) => {
                        p.pos += 1;
                        n == "1"
                    }
                    _ => p.col_ref()?.column.eq_ignore_ascii_case(&first_name),
                };
                if !named {
                    return Err(SqlError::NotTranslated(
                        d::UNION_NOT_TRANSLATED,
                        format!(
                            "ORDER BY of a UNION by anything but its column ('{first_name}', or 1)"
                        ),
                    ));
                }
                let desc = p.kw("DESC");
                if !desc {
                    p.kw("ASC");
                }
                tail.push_str(" @| sort");
                if desc {
                    tail.push_str(" @| reverse");
                }
            }
            if p.kw("LIMIT") {
                match p.peek().cloned() {
                    Some(Tok::Num(n)) => {
                        p.pos += 1;
                        write!(tail, " @| [..{n}]").unwrap();
                    }
                    _ => return Err(SqlError::Syntax("LIMIT takes a number".into())),
                }
            }
        }
        if p.peek().is_some() {
            return Err(shape());
        }
        if all && tables.iter().any(|t| t.eq_ignore_ascii_case(&table)) {
            return Err(SqlError::NotTranslated(
                d::UNION_NOT_TRANSLATED,
                format!(
                    "UNION ALL over the table '{table}' twice (a row both SELECTs take would \
                 answer once: a branch union keeps one thread for a node)"
                ),
            ));
        }
        tables.push(table.clone());
        let mut branch = format!("/{}/*", quarb_key(&table)?);
        if let Some(c) = &cond {
            write!(branch, "[{}]", emit_cond(c, true, &scope, &mut notes)?).unwrap();
        }
        write!(branch, "{}", scope.operand(&col)?).unwrap();
        branches.push(branch);
    }
    let mut q = branches.join(" || ");
    if !all {
        q.push_str(" @| unique");
    }
    q.push_str(&tail);
    Ok(Translation { query: q, notes })
}

/// One `SELECT`, its tokens lexed.
fn translate_select(toks: Vec<Tok>) -> Result<Translation, SqlError> {
    let mut p = Parser { toks, pos: 0 };
    let mut notes = Vec::new();

    p.expect_kw("SELECT").map_err(|_| {
        SqlError::NotTranslated(
            d::STATEMENT_NOT_TRANSLATED,
            "only SELECT statements translate".into(),
        )
    })?;
    let distinct = p.kw("DISTINCT");

    // The select list.
    let mut items = Vec::new();
    loop {
        if p.sym('*') {
            items.push(SelectItem::Star);
        } else if let Some(Tok::Word(w)) = p.peek().cloned() {
            if let Some(a) = agg_kw(&w)
                && matches!(p.toks.get(p.pos + 1), Some(Tok::Sym('(')))
            {
                p.pos += 1; // the name
                let (a, col) = p.agg_call(a)?;
                let alias = p.kw("AS").then(|| p.ident()).transpose()?;
                items.push(SelectItem::Agg(a, col, alias));
            } else {
                let col = p.col_ref()?;
                let alias = p.kw("AS").then(|| p.ident()).transpose()?;
                items.push(SelectItem::Col(col, alias));
            }
        } else {
            return Err(SqlError::Syntax("expected a select item".into()));
        }
        if !p.sym(',') {
            break;
        }
    }

    p.expect_kw("FROM")?;
    let from_table = p.ident()?;
    // Keywords that end the FROM clause and so cannot be a bare
    // table alias.
    const CLAUSE_KEYWORDS: &[&str] = &[
        "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER", "ON", "WHERE", "GROUP",
        "ORDER", "LIMIT", "HAVING", "UNION",
    ];
    let from_alias = if p.kw("AS") {
        Some(p.ident()?)
    } else {
        match p.peek() {
            Some(Tok::Word(w)) if !CLAUSE_KEYWORDS.contains(&w.to_ascii_uppercase().as_str()) => {
                Some(p.ident()?)
            }
            _ => None,
        }
    };

    // One optional JOIN. An inner join is every pair that satisfies
    // its ON (`<=>*`); LEFT keeps the FROM table's rows that have no
    // pair, the joined side null (`<=>*?`); RIGHT is LEFT with the
    // tables' places exchanged; CROSS is every pair. FULL has no
    // correlation that keeps the unmatched rows of both tables.
    if p.kw("FULL") {
        return Err(SqlError::NotTranslated(
            d::JOIN_NOT_TRANSLATED,
            "a FULL JOIN (no correlation keeps the unmatched rows of both tables)".into(),
        ));
    }
    let outer = if p.kw("LEFT") {
        Some(JoinKind::Left)
    } else if p.kw("RIGHT") {
        Some(JoinKind::Right)
    } else {
        None
    };
    if outer.is_some() {
        p.kw("OUTER");
    }
    let cross = outer.is_none() && p.kw("CROSS");
    let mut join = None;
    let mut join_on = None;
    let mut kind = JoinKind::Inner;
    if outer.is_some()
        || cross
        || p.kw("INNER")
        || matches!(p.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("JOIN"))
    {
        p.expect_kw("JOIN")?;
        let t = p.ident()?;
        let alias = if p.kw("AS") {
            Some(p.ident()?)
        } else {
            match p.peek() {
                Some(Tok::Word(w))
                    if !CLAUSE_KEYWORDS.contains(&w.to_ascii_uppercase().as_str()) =>
                {
                    Some(p.ident()?)
                }
                _ => None,
            }
        };
        if cross {
            kind = JoinKind::Cross;
            if matches!(p.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("ON")) {
                return Err(SqlError::NotTranslated(
                    d::JOIN_NOT_TRANSLATED,
                    "CROSS JOIN with an ON clause (a join with a condition is JOIN … ON)".into(),
                ));
            }
        } else {
            kind = outer.unwrap_or(JoinKind::Inner);
            p.expect_kw("ON")?;
            join_on = Some(p.cond()?);
        }
        join = Some((t, alias));
        if matches!(p.peek(), Some(Tok::Word(w))
            if ["JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS"]
                .contains(&w.to_ascii_uppercase().as_str()))
        {
            return Err(SqlError::NotTranslated(
                d::JOIN_NOT_TRANSLATED,
                "more than one JOIN (chain resolutions with '-->' instead)".into(),
            ));
        }
    }
    // RIGHT JOIN: the joined table drives, and the FROM table is the
    // side that may be missing.
    let (from_table, from_alias, join) = match (kind, join) {
        (JoinKind::Right, Some((t, alias))) => {
            kind = JoinKind::Left;
            (t, alias, Some((from_table, from_alias)))
        }
        (_, join) => (from_table, from_alias, join),
    };

    let scope = Scope {
        from: (from_table.clone(), from_alias),
        join: join.clone(),
        on_clause: false,
    };

    let where_cond = p.kw("WHERE").then(|| p.cond()).transpose()?;
    let group_by = p
        .kw("GROUP")
        .then(|| {
            p.expect_kw("BY")?;
            p.col_ref()
        })
        .transpose()?;
    let having = p.kw("HAVING").then(|| p.cond()).transpose()?;
    let order_by = p
        .kw("ORDER")
        .then(|| -> Result<(ColRef, bool), SqlError> {
            p.expect_kw("BY")?;
            let c = p.col_ref()?;
            let desc = p.kw("DESC");
            if !desc {
                p.kw("ASC");
            }
            Ok((c, desc))
        })
        .transpose()?;
    let limit = p
        .kw("LIMIT")
        .then(|| match p.peek().cloned() {
            Some(Tok::Num(n)) => {
                p.pos += 1;
                Ok(n)
            }
            _ => Err(SqlError::Syntax("LIMIT takes a number".into())),
        })
        .transpose()?;
    if let Some(t) = p.peek() {
        return Err(SqlError::NotTranslated(
            d::CONSTRUCT_NOT_TRANSLATED,
            format!("trailing SQL after the query ({t:?})"),
        ));
    }
    if order_by.is_some() {
        notes.push(
            "ORDER BY: a NULL key sorts last, first under DESC (as PostgreSQL orders it; \
             SQLite and MySQL put it first)"
                .to_string(),
        );
    }
    if group_by.is_none() && having.is_some() {
        return Err(SqlError::NotTranslated(
            d::HAVING_NOT_TRANSLATED,
            "HAVING without GROUP BY (a whole-table group)".into(),
        ));
    }

    // ---- emit ----
    let mut q = String::new();
    if let Some((jt, _)) = &join {
        // `SELECT *` over a join is the columns of both tables; the
        // row nodes would be the driver's alone.
        if items.iter().any(|i| matches!(i, SelectItem::Star)) {
            return Err(SqlError::NotTranslated(
                d::JOIN_NOT_TRANSLATED,
                "SELECT * over a JOIN (name the columns: a row node is one table's row)".into(),
            ));
        }
        // The driver is the correlation context; the joined table
        // the result context, its ON in the bracket, where the
        // driver is `$$0`. Every pair is a thread (`<=>*`), as every
        // pair is a row; `<=>*?` keeps a driver with no pair.
        let arrow = if kind == JoinKind::Left {
            "<=>*?"
        } else {
            "<=>*"
        };
        write!(
            q,
            "/{}/* {arrow} /{}/*",
            quarb_key(&from_table)?,
            quarb_key(jt)?
        )
        .unwrap();
        let mut inside: Vec<String> = Vec::new();
        if let Some(on) = &join_on {
            inside.push(emit_cond(on, true, &scope.on(), &mut notes)?);
        }
        // WHERE reads the joined rows. For an inner join that is the
        // ON's own filter, and it rides the bracket (where a source
        // takes the whole as one JOIN); after an outer join it must
        // see the rows the join kept with no pair, and a test of the
        // driver's column for NULL cannot stand in a bracket: there
        // it follows the join as a filter.
        let after = where_cond
            .as_ref()
            .filter(|w| kind == JoinKind::Left || scope.tests_driver_null(w));
        if after.is_none()
            && let Some(w) = &where_cond
        {
            inside.push(emit_cond(w, true, &scope.on(), &mut notes)?);
        }
        if !inside.is_empty() {
            write!(q, "[{}]", inside.join(" && ")).unwrap();
        }
        if let Some(w) = after {
            write!(q, " | [{}]", emit_cond(w, true, &scope, &mut notes)?).unwrap();
        }
    } else {
        write!(q, "/{}/*", quarb_key(&from_table)?).unwrap();
        if let Some(w) = &where_cond {
            write!(q, "[{}]", emit_cond(w, true, &scope, &mut notes)?).unwrap();
        }
    }

    // GROUP BY: the aggregate rides the plain pipe.
    if let Some(k) = &group_by {
        if distinct {
            return Err(SqlError::NotTranslated(
                d::DISTINCT_NOT_TRANSLATED,
                "SELECT DISTINCT with GROUP BY".into(),
            ));
        }
        let aggs: Vec<&SelectItem> = items
            .iter()
            .filter(|i| matches!(i, SelectItem::Agg(..)))
            .collect();
        if aggs.len() != 1 {
            return Err(SqlError::NotTranslated(
                d::GROUP_NOT_TRANSLATED,
                "GROUP BY translates with exactly one aggregate in the select list".into(),
            ));
        }
        let SelectItem::Agg(a, col, alias) = aggs[0] else {
            unreachable!()
        };
        // Any plain select item must be the group key — anything
        // else would silently vanish from the translation.
        let mut key_alias = None;
        for item in &items {
            match item {
                SelectItem::Col(c, ka) if c.column.eq_ignore_ascii_case(&k.column) => {
                    key_alias = ka.clone();
                }
                SelectItem::Col(c, _) => {
                    return Err(SqlError::NotTranslated(
                        d::GROUP_NOT_TRANSLATED,
                        format!(
                            "the non-aggregate column '{}' is not the GROUP BY key",
                            c.column
                        ),
                    ));
                }
                SelectItem::Star => {
                    return Err(SqlError::NotTranslated(
                        d::GROUP_NOT_TRANSLATED,
                        "SELECT * with GROUP BY".into(),
                    ));
                }
                SelectItem::Agg(..) => {}
            }
        }
        // The key is pushed under its name (the alias, else the
        // column) and the reduction folds by it: `| .k(::k) @|{.k}
        // sum`. SQL's NULL-key group is the fold's null partition.
        let key_name = key_alias.clone().unwrap_or_else(|| k.column.clone());
        if !plain_register(&key_name) {
            return Err(SqlError::NotTranslated(
                d::NAME_NOT_TRANSLATED,
                format!("the GROUP BY key {key_name:?} (not a plain register name)"),
            ));
        }
        write!(q, " | .{key_name}({})", scope.operand(k)?).unwrap();
        // The column an aggregate reads is piped to it; COUNT(col)
        // counts its values other than NULL, `count(--values)`.
        if let Some(c) = col {
            write!(q, " | {}", scope.operand(c)?).unwrap();
        }
        let name = alias.clone().unwrap_or_else(|| agg_word(a).to_string());
        if !plain_register(&name) {
            return Err(SqlError::NotTranslated(
                d::NAME_NOT_TRANSLATED,
                format!("the aggregate alias {name:?} (not a plain register name)"),
            ));
        }
        write!(q, " @|{{.{key_name}}} {} | .{name}", agg_fn(a)).unwrap();
        if let Some(h) = &having {
            // HAVING refers to the aggregate (an aggregate call,
            // its alias, or its function name) or the group key.
            let key_field = key_alias.as_deref().unwrap_or(&k.column);
            let cond = emit_having(h, a, col.as_ref(), &name, &k.column, key_field)?;
            write!(q, " | [{cond}]").unwrap();
        }
        write!(q, " | %.").unwrap();
        // ORDER BY / LIMIT after the grouped form: the sort key is
        // a field of the group record, the key or the aggregate.
        if let Some((c, desc)) = &order_by {
            let field = if c.column.eq_ignore_ascii_case(&k.column) {
                key_name.clone()
            } else if c.column.eq_ignore_ascii_case(&name) {
                name.clone()
            } else {
                return Err(SqlError::NotTranslated(
                    d::SORT_NOT_TRANSLATED,
                    format!(
                        "ORDER BY '{}': neither the GROUP BY key nor the aggregate",
                        c.column
                    ),
                ));
            };
            write!(q, " @| sort_by(:{field})").unwrap();
            if *desc {
                q.push_str(" @| reverse");
            }
        }
        if let Some(n) = &limit {
            write!(q, " @| [..{n}]").unwrap();
        }
        return Ok(Translation { query: q, notes });
    } else if items.iter().any(|i| matches!(i, SelectItem::Agg(..))) {
        // Bare aggregates over the whole table.
        if items.len() != 1 {
            return Err(SqlError::NotTranslated(
                d::GROUP_NOT_TRANSLATED,
                "mixing aggregates and columns without GROUP BY".into(),
            ));
        }
        if distinct {
            return Err(SqlError::NotTranslated(
                d::DISTINCT_NOT_TRANSLATED,
                "SELECT DISTINCT with an aggregate".into(),
            ));
        }
        let SelectItem::Agg(a, col, _) = &items[0] else {
            unreachable!()
        };
        // The column an aggregate reads is piped to it; COUNT(col)
        // counts its values other than NULL, `count(--values)`.
        if let Some(c) = col {
            write!(q, " | {}", scope.operand(c)?).unwrap();
        }
        write!(q, " @| {}", agg_fn(a)).unwrap();
    } else {
        // Plain column selection: sort, then DISTINCT's dedup
        // (stable, so the sorted order survives), then LIMIT —
        // SQL applies DISTINCT before ORDER BY and LIMIT.
        if let Some((c, desc)) = &order_by {
            write!(q, " @| sort_by({})", scope.operand(c)?).unwrap();
            if *desc {
                q.push_str(" @| reverse");
            }
        }
        if distinct {
            match items.as_slice() {
                [SelectItem::Star] => {
                    return Err(SqlError::NotTranslated(
                        d::DISTINCT_NOT_TRANSLATED,
                        "SELECT DISTINCT *".into(),
                    ));
                }
                [SelectItem::Col(c, _)] => {
                    write!(q, " | {} @| unique", scope.operand(c)?).unwrap();
                }
                // several columns: the rows as records, of which
                // `unique` keeps the first of each (two NULLs are
                // one value there, as they are under DISTINCT)
                _ => {
                    let fields = record_fields(&items, &scope)?;
                    write!(q, " | %({}) @| unique", fields.join("; ")).unwrap();
                    notes.push("the result streams as records (JSONL), not a table".to_string());
                }
            }
            if let Some(n) = &limit {
                write!(q, " @| [..{n}]").unwrap();
            }
            return Ok(Translation { query: q, notes });
        }
        if let Some(n) = &limit {
            write!(q, " @| [..{n}]").unwrap();
        }
        if items.len() == 1 && matches!(items[0], SelectItem::Star) {
            // row nodes as-is
            notes.push("SELECT *: the result is the row nodes (their locators print)".to_string());
        } else {
            let fields = record_fields(&items, &scope)?;
            write!(q, " | %({})", fields.join("; ")).unwrap();
            notes.push("the result streams as records (JSONL), not a table".to_string());
        }
        return Ok(Translation { query: q, notes });
    }

    // ORDER BY / LIMIT after the bare aggregate form.
    if let Some((c, desc)) = &order_by {
        write!(q, " @| sort_by(::{})", quarb_key(&c.column)?).unwrap();
        if *desc {
            q.push_str(" @| reverse");
        }
    }
    if let Some(n) = &limit {
        write!(q, " @| [..{n}]").unwrap();
    }
    Ok(Translation { query: q, notes })
}

/// The fields of the record a select list of columns projects: a
/// column under its own name, an aliased one under its alias, and a
/// joined table's column under its SQL qualifier, so that the two
/// sides of a join keep distinct keys.
fn record_fields(items: &[SelectItem], scope: &Scope) -> Result<Vec<String>, SqlError> {
    let mut fields = Vec::new();
    for item in items {
        match item {
            SelectItem::Star => {
                return Err(SqlError::NotTranslated(
                    d::CONSTRUCT_NOT_TRANSLATED,
                    "mixing * with named columns".into(),
                ));
            }
            SelectItem::Col(c, alias) => {
                let op = scope.operand(c)?;
                match alias {
                    Some(a) => fields.push(format!("{} = {op}", quarb_str(a))),
                    None if op.starts_with("$$")
                        && op[2..].starts_with(|c: char| c.is_ascii_digit()) =>
                    {
                        let name = match &c.table {
                            Some(t) => format!("{t}.{}", c.column),
                            None => c.column.clone(),
                        };
                        fields.push(format!("{} = {op}", quarb_str(&name)));
                    }
                    None => fields.push(op),
                }
            }
            SelectItem::Agg(..) => {
                return Err(SqlError::NotTranslated(
                    d::GROUP_NOT_TRANSLATED,
                    "an aggregate beside plain columns in this position".into(),
                ));
            }
        }
    }
    Ok(fields)
}

/// A name usable as a bare Quarb register (`.name` / `$.name`).
fn plain_register(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// A HAVING condition: a single comparison whose left side names
/// the (single) aggregate — an aggregate call matching the select
/// list's, its alias, or its function name (it is the topic's
/// pushed register, `$_`) — or the group key (`$.key`).
fn emit_having(
    c: &Cond,
    agg: &Agg,
    agg_col: Option<&ColRef>,
    agg_name: &str,
    key: &str,
    key_field: &str,
) -> Result<String, SqlError> {
    let rhs_text = |rhs: &Scalar| -> Result<String, SqlError> {
        match rhs {
            Scalar::Col(_) => Err(SqlError::NotTranslated(
                d::HAVING_NOT_TRANSLATED,
                "HAVING compares against a literal".into(),
            )),
            Scalar::Str(v) => Ok(quarb_str(v)),
            Scalar::Num(n) => Ok(n.clone()),
            Scalar::Null => Ok("null".to_string()),
        }
    };
    match c {
        Cond::AggCmp(a, col, op, rhs) => {
            let same_col = match (col, agg_col) {
                (None, None) => true,
                (Some(x), Some(y)) => x.column.eq_ignore_ascii_case(&y.column),
                _ => false,
            };
            if a != agg || !same_col {
                return Err(SqlError::NotTranslated(
                    d::HAVING_NOT_TRANSLATED,
                    "HAVING refers to an aggregate not in the select list".into(),
                ));
            }
            Ok(format!("$_ {op} {}", rhs_text(rhs)?))
        }
        Cond::Cmp(col, op, rhs) => {
            let lhs = if col.column.eq_ignore_ascii_case(agg_name) {
                "$_".to_string()
            } else if col.column.eq_ignore_ascii_case(key) {
                if !plain_register(key_field) {
                    return Err(SqlError::NotTranslated(
                        d::NAME_NOT_TRANSLATED,
                        format!(
                            "the group key {key_field:?} in HAVING (not a plain register name)"
                        ),
                    ));
                }
                format!("$.{key_field}")
            } else {
                return Err(SqlError::NotTranslated(
                    d::HAVING_NOT_TRANSLATED,
                    format!(
                        "the HAVING column '{}' (name the aggregate or the group key)",
                        col.column
                    ),
                ));
            };
            Ok(format!("{lhs} {op} {}", rhs_text(rhs)?))
        }
        _ => Err(SqlError::NotTranslated(
            d::HAVING_NOT_TRANSLATED,
            "HAVING translates for a single comparison".into(),
        )),
    }
}
