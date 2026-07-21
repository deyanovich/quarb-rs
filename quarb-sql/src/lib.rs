//! SQL importer for Quarb.
//!
//! Translates a SQL `SELECT` statement into an equivalent Quarb
//! query, following the mapping in the SQL cookbook — in reverse.
//! The translatable subset covers the single-statement query core;
//! everything else is refused with an
//! [`SqlError::Unsupported`] naming the construct, never silently
//! approximated.
//!
//! - `SELECT cols FROM t` → `/t/* | rec(::col, …)`; `SELECT *` →
//!   the row nodes; `SELECT DISTINCT col` → `/t/*::col @| unique`.
//! - `WHERE` → a predicate: comparisons, `AND`/`OR`/`NOT`,
//!   `IS [NOT] NULL` → `= null` / `!= null` (exact: Quarb's
//!   `value_eq` treats NULL as an ordinary value), `IN (…)` → an
//!   `or`-chain, `LIKE` → anchored `=~` regexes.
//! - Aggregates (`COUNT(*)`, `COUNT(col)`, `SUM`/`AVG`/`MIN`/`MAX`)
//!   → `@|` reductions; `GROUP BY k` → `@| group(::k)` with the
//!   aggregate riding the plain pipe and `HAVING` a filter after
//!   it.
//! - `JOIN t2 ON t2.b = t1.a` → correlation: `/t1/* <=>
//!   /t2/*[::b = $*1::a]`, with two-sided select lists projected
//!   through the witness (`$*1::col`).
//! - `ORDER BY` → `@| sort_by(…)` (`DESC` via `@| reverse`, or a
//!   numeric `-` key when mixed); `LIMIT n` → `@| [..n]`.
//!
//! Refused: subqueries, `UNION`, window functions, `CASE`,
//! outer and `CROSS` joins (Quarb's `<=>` correlation is
//! inner/existential), multi-join chains (one `JOIN` translates),
//! `HAVING` without `GROUP BY`, aggregate arithmetic
//! (`SUM(a) + 1`), and any non-`SELECT` statement.
//!
//! Known semantic divergences are reported as
//! [`Translation::notes`] rather than errors:
//!
//! - Quarb's `count` counts all rows (SQL `COUNT(col)` skips NULLs;
//!   the translation adds the `[::col != null]` filter, and says
//!   so).
//! - Quarb streams records (JSONL), not a result table.
//! - `LIKE` translates to a case-insensitive regex (matching
//!   MySQL's and SQLite's default folding; PostgreSQL's `LIKE`
//!   is case-sensitive).
//! - SQL keeps a NULL-key group under `GROUP BY`; Quarb's `group`
//!   drops null keys.

mod export;
pub use export::{
    Dialect, Partial, Pushdown, export, partial_pushdown, partial_pushdown_explained, pushdown,
    pushdown_explained,
};

use std::fmt::Write as _;

/// An error translating a SQL statement.
#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error("SQL syntax error: {0}")]
    Syntax(String),
    #[error("unsupported SQL construct: {0}")]
    Unsupported(String),
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
        if self.kw("LIKE") {
            match self.peek().cloned() {
                Some(Tok::Str(p)) => {
                    self.pos += 1;
                    return Ok(Cond::Like(col, p));
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
            return Ok(Cond::In(col, items));
        }
        let op = self.cmp_op()?;
        let rhs = self.scalar()?;
        Ok(Cond::Cmp(col, op, rhs))
    }
}

// ---------------------------------------------------------------
// Emission
// ---------------------------------------------------------------

/// The tables in scope: (name, alias). The first is the FROM table
/// (correlation context `$*1` when a JOIN is present); the second
/// the JOIN table.
struct Scope {
    from: (String, Option<String>),
    join: Option<(String, Option<String>)>,
}

impl Scope {
    /// Whether `col` belongs to the FROM (left) table: qualified by
    /// its name or alias, or unqualified with no join.
    fn is_left(&self, col: &ColRef) -> Result<bool, SqlError> {
        let Some(q) = &col.table else {
            if self.join.is_some() {
                return Err(SqlError::Unsupported(format!(
                    "unqualified column '{}' in a JOIN (qualify it)",
                    col.column
                )));
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

    /// The operand for `col`: `::col`, or `$*1::col` for the left
    /// side under a join (whose result context is the right side).
    fn operand(&self, col: &ColRef) -> Result<String, SqlError> {
        let key = quarb_key(&col.column)?;
        Ok(if self.join.is_some() && self.is_left(col)? {
            format!("$*1::{key}")
        } else {
            format!("::{key}")
        })
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
        return Err(SqlError::Unsupported(format!(
            "the identifier {name:?} (a quote inside a quoted identifier)"
        )));
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

fn emit_cond(c: &Cond, scope: &Scope, notes: &mut Vec<String>) -> Result<String, SqlError> {
    Ok(match c {
        Cond::Cmp(col, op, rhs) => {
            let lhs = scope.operand(col)?;
            format!("{lhs} {op} {}", scalar_text(rhs, scope)?)
        }
        Cond::AggCmp(..) => {
            return Err(SqlError::Unsupported(
                "an aggregate in WHERE (SQL puts it in HAVING)".into(),
            ));
        }
        Cond::Like(col, pat) => {
            let lhs = scope.operand(col)?;
            let inner = pat.trim_matches('%');
            if inner.contains('%') || inner.contains('_') {
                return Err(SqlError::Unsupported(format!(
                    "LIKE pattern '{pat}' (only simple %x%, x%, %x forms translate)"
                )));
            }
            notes.push(
                "LIKE: translated to a case-insensitive regex (SQL's default \
                 ASCII case folding)"
                    .to_string(),
            );
            let esc = regex_escape(inner);
            match (pat.starts_with('%'), pat.ends_with('%')) {
                (true, true) => format!("{lhs} =~ /(?i){esc}/"),
                (false, true) => format!("{lhs} =~ /(?i)^{esc}/"),
                (true, false) => format!("{lhs} =~ /(?i){esc}$/"),
                (false, false) => format!("{lhs} =~ /(?i)^{esc}$/"),
            }
        }
        Cond::IsNull(col, is_null) => {
            // `= null` / `!= null`: Quarb's `value_eq` treats NULL
            // as an ordinary value, so these are exactly SQL's
            // `IS [NOT] NULL` — truthiness (`not ::c`) would also
            // drop 0 and ''.
            let lhs = scope.operand(col)?;
            if *is_null {
                format!("{lhs} = null")
            } else {
                format!("{lhs} != null")
            }
        }
        Cond::In(col, items) => {
            let lhs = scope.operand(col)?;
            let parts: Vec<String> = items
                .iter()
                .map(|s| Ok(format!("{lhs} = {}", scalar_text(s, scope)?)))
                .collect::<Result<_, SqlError>>()?;
            format!("({})", parts.join(" or "))
        }
        Cond::And(a, b) => format!(
            "{} and {}",
            emit_cond(a, scope, notes)?,
            emit_cond(b, scope, notes)?
        ),
        Cond::Or(a, b) => format!(
            "({} or {})",
            emit_cond(a, scope, notes)?,
            emit_cond(b, scope, notes)?
        ),
        Cond::Not(a) => format!("not ({})", emit_cond(a, scope, notes)?),
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

fn agg_fn(a: &Agg) -> &'static str {
    match a {
        Agg::Count | Agg::CountCol => "count",
        Agg::Sum => "sum",
        Agg::Avg => "mean",
        Agg::Min => "min",
        Agg::Max => "max",
    }
}

/// Translate one SQL `SELECT` statement to a Quarb query.
pub fn translate(sql: &str) -> Result<Translation, SqlError> {
    let toks = lex(sql.trim().trim_end_matches(';'))?;
    let mut p = Parser { toks, pos: 0 };
    let mut notes = Vec::new();

    p.expect_kw("SELECT")
        .map_err(|_| SqlError::Unsupported("only SELECT statements translate".into()))?;
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

    // One optional inner JOIN ... ON a = b; the outer forms have
    // no existential-correlation equivalent, so they refuse rather
    // than silently translating as inner.
    if p.kw("LEFT") || p.kw("RIGHT") || p.kw("FULL") {
        return Err(SqlError::Unsupported(
            "an outer JOIN (Quarb's '<=>' correlation is inner/existential)".into(),
        ));
    }
    if p.kw("CROSS") {
        return Err(SqlError::Unsupported("CROSS JOIN".into()));
    }
    let mut join = None;
    let mut join_on = None;
    if p.kw("INNER") || matches!(p.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("JOIN")) {
        p.expect_kw("JOIN")?;
        let t = p.ident()?;
        let alias = if p.kw("AS") {
            Some(p.ident()?)
        } else {
            match p.peek() {
                Some(Tok::Word(w)) if !w.eq_ignore_ascii_case("ON") => Some(p.ident()?),
                _ => None,
            }
        };
        p.expect_kw("ON")?;
        let l = p.col_ref()?;
        if !p.sym('=') {
            return Err(SqlError::Unsupported("non-equi JOIN".into()));
        }
        let r = p.col_ref()?;
        join = Some((t, alias));
        join_on = Some((l, r));
        if matches!(p.peek(), Some(Tok::Word(w))
            if ["JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS"]
                .contains(&w.to_ascii_uppercase().as_str()))
        {
            return Err(SqlError::Unsupported(
                "more than one JOIN (chain resolutions with '~>' instead)".into(),
            ));
        }
    }

    let scope = Scope {
        from: (from_table.clone(), from_alias),
        join: join.clone(),
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
        return Err(SqlError::Unsupported(format!(
            "trailing SQL after the query ({t:?})"
        )));
    }
    if group_by.is_none() && having.is_some() {
        return Err(SqlError::Unsupported(
            "HAVING without GROUP BY (a whole-table group)".into(),
        ));
    }

    // ---- emit ----
    let mut q = String::new();
    if let Some((jt, _)) = &join {
        let (l, r) = join_on.as_ref().expect("join has ON");
        // The left/FROM table becomes the correlation context; the
        // joined table the result context.
        let (left_col, right_col) = if scope.is_left(l)? { (l, r) } else { (r, l) };
        write!(
            q,
            "/{}/* <=> /{}/*[::{} = $*1::{}",
            quarb_key(&from_table)?,
            quarb_key(jt)?,
            quarb_key(&right_col.column)?,
            quarb_key(&left_col.column)?
        )
        .unwrap();
        if let Some(w) = &where_cond {
            write!(q, " and {}", emit_cond(w, &scope, &mut notes)?).unwrap();
        }
        q.push(']');
        notes.push(
            "JOIN: existential semantics — one result row per joined-table row, \
             bound to its first witness"
                .to_string(),
        );
    } else {
        write!(q, "/{}/*", quarb_key(&from_table)?).unwrap();
        if let Some(w) = &where_cond {
            write!(q, "[{}]", emit_cond(w, &scope, &mut notes)?).unwrap();
        }
    }

    // GROUP BY: the aggregate rides the plain pipe.
    if let Some(k) = &group_by {
        if distinct {
            return Err(SqlError::Unsupported("SELECT DISTINCT with GROUP BY".into()));
        }
        let aggs: Vec<&SelectItem> = items
            .iter()
            .filter(|i| matches!(i, SelectItem::Agg(..)))
            .collect();
        if aggs.len() != 1 {
            return Err(SqlError::Unsupported(
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
                    return Err(SqlError::Unsupported(format!(
                        "the non-aggregate column '{}' is not the GROUP BY key",
                        c.column
                    )));
                }
                SelectItem::Star => {
                    return Err(SqlError::Unsupported("SELECT * with GROUP BY".into()));
                }
                SelectItem::Agg(..) => {}
            }
        }
        notes.push(
            "GROUP BY: SQL keeps a NULL-key group; Quarb's group drops null keys".to_string(),
        );
        if let Some(c) = col {
            let op = scope.operand(c)?;
            if matches!(a, Agg::CountCol) {
                notes.push(format!(
                    "COUNT({}): Quarb count counts all; the [{op} != null] filter \
                     restores SQL's NULL-skipping",
                    c.column
                ));
                write!(q, "[{op} != null]").unwrap();
            }
            if !matches!(a, Agg::Count | Agg::CountCol) {
                write!(q, " | {op}").unwrap();
            }
        }
        match &key_alias {
            Some(ka) => {
                write!(q, " @| group({}, {})", quarb_str(ka), scope.operand(k)?).unwrap()
            }
            None => write!(q, " @| group({})", scope.operand(k)?).unwrap(),
        }
        let name = alias.clone().unwrap_or_else(|| agg_fn(a).to_string());
        if !plain_register(&name) {
            return Err(SqlError::Unsupported(format!(
                "the aggregate alias {name:?} (not a plain register name)"
            )));
        }
        write!(q, " | {} | .{name}", agg_fn(a)).unwrap();
        if let Some(h) = &having {
            // HAVING refers to the aggregate (an aggregate call,
            // its alias, or its function name) or the group key.
            let key_field = key_alias.as_deref().unwrap_or(&k.column);
            let cond = emit_having(h, a, col.as_ref(), &name, &k.column, key_field)?;
            write!(q, " | [{cond}]").unwrap();
        }
        write!(q, " | %.").unwrap();
    } else if items.iter().any(|i| matches!(i, SelectItem::Agg(..))) {
        // Bare aggregates over the whole table.
        if items.len() != 1 {
            return Err(SqlError::Unsupported(
                "mixing aggregates and columns without GROUP BY".into(),
            ));
        }
        if distinct {
            return Err(SqlError::Unsupported(
                "SELECT DISTINCT with an aggregate".into(),
            ));
        }
        let SelectItem::Agg(a, col, _) = &items[0] else {
            unreachable!()
        };
        if let Some(c) = col {
            let op = scope.operand(c)?;
            if matches!(a, Agg::CountCol) {
                notes.push(format!(
                    "COUNT({}): Quarb count counts all; the [{op} != null] filter \
                     restores SQL's NULL-skipping",
                    c.column
                ));
                write!(q, "[{op} != null]").unwrap();
            }
            if !matches!(a, Agg::Count | Agg::CountCol) {
                write!(q, " | {op}").unwrap();
            }
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
            if items.len() != 1 {
                return Err(SqlError::Unsupported(
                    "SELECT DISTINCT translates for a single column".into(),
                ));
            }
            let SelectItem::Col(c, _) = &items[0] else {
                return Err(SqlError::Unsupported("SELECT DISTINCT *".into()));
            };
            write!(q, " | {} @| unique", scope.operand(c)?).unwrap();
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
            let mut fields = Vec::new();
            for item in &items {
                match item {
                    SelectItem::Star => {
                        return Err(SqlError::Unsupported("mixing * with named columns".into()));
                    }
                    SelectItem::Col(c, alias) => {
                        let op = scope.operand(c)?;
                        match alias {
                            Some(a) => fields.push(format!("{}, {op}", quarb_str(a))),
                            // The witness side names its field with
                            // the SQL qualifier, so two-sided select
                            // lists keep distinct keys.
                            None if op.starts_with("$*") => {
                                let name = match &c.table {
                                    Some(t) => format!("{t}.{}", c.column),
                                    None => c.column.clone(),
                                };
                                fields.push(format!("{}, {op}", quarb_str(&name)));
                            }
                            None => fields.push(op),
                        }
                    }
                    SelectItem::Agg(..) => unreachable!("handled above"),
                }
            }
            write!(q, " | rec({})", fields.join(", ")).unwrap();
            notes.push("the result streams as records (JSONL), not a table".to_string());
        }
        return Ok(Translation { query: q, notes });
    }

    // ORDER BY / LIMIT after grouped or aggregate forms; the sort
    // key names a field of the group record.
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
            Scalar::Col(_) => Err(SqlError::Unsupported(
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
                return Err(SqlError::Unsupported(
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
                    return Err(SqlError::Unsupported(format!(
                        "the group key {key_field:?} in HAVING (not a plain register name)"
                    )));
                }
                format!("$.{key_field}")
            } else {
                return Err(SqlError::Unsupported(format!(
                    "the HAVING column '{}' (name the aggregate or the group key)",
                    col.column
                )));
            };
            Ok(format!("{lhs} {op} {}", rhs_text(rhs)?))
        }
        _ => Err(SqlError::Unsupported(
            "HAVING translates for a single comparison".into(),
        )),
    }
}
