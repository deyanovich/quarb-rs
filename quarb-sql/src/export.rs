//! The reverse direction: Quarb → SQL.
//!
//! Walks the query's *reflection arbor* — the locked vocabulary is
//! the stable surface for query-rewriting tooling — and emits an
//! equivalent `SELECT` statement, refusing what the SQL query core
//! cannot express. Fragments and pure macros expand before
//! reflection, so they translate for free.
//!
//! The translatable subset mirrors the importer: `/table/*`
//! branches with predicates (→ `WHERE`), witness joins (`<=>` with
//! `$*1` equality → `JOIN ... ON`, remaining conditions → `WHERE`,
//! `$*k` select fields qualified), `rec(...)` select lists,
//! whole-table and grouped aggregates (`GROUP BY`, a filter after
//! the reduction → `HAVING`), `sort_by`/`reverse`/`top` →
//! `ORDER BY`/`LIMIT`, `@| [..n]` → `LIMIT`, and single-column
//! `unique` → `DISTINCT`.
//!
//! Deliberately refused: `~>` resolution chains — the foreign-key
//! targets live in the *schema*, not the query text, so a chain
//! cannot be compiled to a join without a database at hand; spell
//! the join explicitly with `<=>` to export it. Also refused:
//! registers beyond the grouped-aggregate pattern, windows,
//! captures, and regex matching (`LIKE` dialects disagree).
//!
//! Notes carry the standing divergences: truthiness (`[::c]` →
//! `IS NOT NULL`, but Quarb also treats `0` and `''` as falsy),
//! `*=` → `LIKE` case-folding dialects, and join cardinality
//! (Quarb's existential binding never multiplies rows; SQL `JOIN`
//! does when several left rows match).

use crate::{SqlError, Translation};
use quarb::reflect::QueryArbor;
use quarb::{AstAdapter, NodeId, Value};

/// Translate a Quarb query to a SQL `SELECT` statement.
pub fn export(quarb: &str) -> Result<Translation, SqlError> {
    refuse_marker(quarb)?;
    let arbor =
        QueryArbor::parse(quarb).map_err(|e| SqlError::Syntax(format!("parsing Quarb: {e}")))?;
    refuse_groups(&arbor)?;
    let mut ex = Exporter {
        arbor,
        notes: Vec::new(),
        strict: false,
        dialect: None,
        from_table: String::new(),
        join_on_left_cols: Vec::new(),
        join_table: None,
        aggregate: false,
    };
    let query = ex.query()?;
    Ok(Translation {
        query,
        notes: ex.notes,
    })
}

/// The exporter rewrites `__LEFT__` (the join's left table) and
/// `__AGG__` (the HAVING aggregate) as internal placeholders.
/// Query text containing either marker would be rewritten inside
/// its own string literals — and could spoof the emitted SQL — so
/// such queries stay on the scan path.
fn refuse_marker(quarb: &str) -> Result<(), SqlError> {
    for marker in ["__LEFT__", "__AGG__"] {
        if quarb.contains(marker) {
            return Err(SqlError::Unsupported(format!(
                "query text contains the reserved marker \"{marker}\""
            )));
        }
    }
    Ok(())
}

/// A pushdown plan: SQL whose execution is provably identical to
/// the Quarb query's — plus the table whose primary key must order
/// the rows (`None` for a single aggregate row, where order is
/// moot). The driver appends the `ORDER BY`, since the key lives in
/// its catalog.
pub struct Pushdown {
    pub sql: String,
    pub order_table: Option<String>,
    /// Present when the plan contains a witness JOIN: the left
    /// table and the left-side columns its ON equalities bind
    /// (collected structurally from the `$*1` operands, so query
    /// text cannot spoof them).
    /// The plan is only sound if those columns form a unique key
    /// of the left table (else SQL multiplies rows where Quarb's
    /// existential binding does not) — the *driver* must verify
    /// against its catalog before executing, and fall back to the
    /// scan if it cannot.
    pub join_left: Option<(String, Vec<String>)>,
}

/// The target SQL dialect, for the one construct whose emitted
/// SQL is not portable: a filter that navigates *into* a JSON
/// column, which each engine extracts with its own operator.
/// The rest of a pushdown is dialect-agnostic. A query with no
/// JSON-column filter emits identical SQL regardless of dialect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Postgres,
    MySql,
    Sqlite,
    Mssql,
    Oracle,
}

/// Attempt the pushdown translation: `Some` only when every
/// construct in the query is in the verified-safe set. Anything
/// else — including everything `export` would merely annotate with
/// a divergence note — returns `None`, and the caller scans.
///
/// `dialect` enables JSON-column-path pushdown for that engine
/// (fixed-path string equality only); `None` keeps such filters
/// on the client-side graft.
pub fn pushdown(quarb: &str, dialect: Option<Dialect>) -> Option<Pushdown> {
    pushdown_explained(quarb, dialect).ok()
}

/// [`pushdown`], keeping the refusal: the error names the first
/// construct that kept the query on the scan path.
pub fn pushdown_explained(quarb: &str, dialect: Option<Dialect>) -> Result<Pushdown, SqlError> {
    refuse_marker(quarb)?;
    let arbor =
        QueryArbor::parse(quarb).map_err(|e| SqlError::Syntax(format!("parsing Quarb: {e}")))?;
    refuse_groups(&arbor)?;
    let mut ex = Exporter {
        arbor,
        notes: Vec::new(),
        strict: true,
        dialect,
        from_table: String::new(),
        join_on_left_cols: Vec::new(),
        join_table: None,
        aggregate: false,
    };
    let sql = ex.query()?;
    let order_table = if ex.aggregate {
        None
    } else {
        // Rows come back in the result context's document order:
        // the joined table's under a correlation, else the FROM
        // table's.
        Some(ex.join_table.clone().unwrap_or(ex.from_table.clone()))
    };
    let join_left = ex
        .join_table
        .is_some()
        .then(|| (ex.from_table.clone(), ex.join_on_left_cols.clone()));
    Ok(Pushdown {
        sql,
        order_table,
        join_left,
    })
}

/// The dialect-specific SQL that extracts a JSON scalar at a
/// fixed object path, unquoted to text. `path` holds plain
/// object keys (identifier-safe, per `json_path`), so no
/// per-dialect path-escaping is needed. Each engine's operator
/// returns the value as text and yields NULL for an absent path
/// or a non-scalar — matching the graft, which excludes those
/// rows too.
fn json_extract(dialect: Dialect, qual: Option<&str>, col: &str, path: &[String]) -> String {
    let qcol = match qual {
        Some(q) => format!("{q}.{col}"),
        None => col.to_string(),
    };
    match dialect {
        // `#>>` takes a text-array path and returns text; the
        // `::jsonb` cast lets it work on json, jsonb, and
        // text-holding-JSON columns alike (an invalid-JSON row
        // errors, and the driver falls back to the scan).
        Dialect::Postgres => format!("({qcol}::jsonb #>> '{{{}}}')", path.join(",")),
        // JSON_UNQUOTE(JSON_EXTRACT(...)) is the `->>` shorthand
        // spelled out — portable across MySQL and MariaDB, where
        // the `->>` operator itself is not.
        Dialect::MySql => {
            format!("JSON_UNQUOTE(JSON_EXTRACT({qcol}, '$.{}'))", path.join("."))
        }
        Dialect::Sqlite => format!("json_extract({qcol}, '$.{}')", path.join(".")),
        // JSON_VALUE returns a scalar as text (lax mode: NULL on a
        // missing path or a non-scalar, no error).
        Dialect::Mssql | Dialect::Oracle => format!("JSON_VALUE({qcol}, '$.{}')", path.join(".")),
    }
}

/// SQL keywords that must not appear as a bare identifier or `AS`
/// alias — quoting them portably differs by dialect, so strict
/// mode refuses and export mode quotes with double quotes plus a
/// note.
const SQL_KEYWORDS: &[&str] = &[
    "all",
    "and",
    "as",
    "asc",
    "by",
    "case",
    "cross",
    "desc",
    "distinct",
    "else",
    "end",
    "except",
    "exists",
    "from",
    "group",
    "having",
    "in",
    "index",
    "inner",
    "intersect",
    "into",
    "is",
    "join",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "right",
    "select",
    "set",
    "table",
    "then",
    "union",
    "unique",
    "update",
    "using",
    "values",
    "when",
    "where",
];

/// A bare SQL identifier, portable across the target dialects: a
/// letter or underscore, then letters, digits, and underscores,
/// and not a reserved word.
fn is_plain_ident(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !SQL_KEYWORDS.contains(&name.to_ascii_lowercase().as_str())
}

/// The SELECT under construction.
#[derive(Default)]
struct Select {
    select: Vec<String>,
    distinct: bool,
    from: String,
    join: Option<(String, String)>, // (table, ON condition)
    wheres: Vec<String>,
    group_by: Option<String>,
    having: Option<String>,
    order_by: Option<(String, bool)>, // (expr, desc)
    limit: Option<String>,
}

impl Select {
    fn render(&self) -> String {
        let mut out = String::from("SELECT ");
        if self.distinct {
            out.push_str("DISTINCT ");
        }
        if self.select.is_empty() {
            out.push('*');
        } else {
            out.push_str(&self.select.join(", "));
        }
        out.push_str(&format!(" FROM {}", self.from));
        if let Some((t, on)) = &self.join {
            out.push_str(&format!(" JOIN {t} ON {on}"));
        }
        if !self.wheres.is_empty() {
            out.push_str(&format!(" WHERE {}", self.wheres.join(" AND ")));
        }
        if let Some(g) = &self.group_by {
            out.push_str(&format!(" GROUP BY {g}"));
        }
        if let Some(h) = &self.having {
            out.push_str(&format!(" HAVING {h}"));
        }
        if let Some((e, desc)) = &self.order_by {
            out.push_str(&format!(" ORDER BY {e}"));
            if *desc {
                out.push_str(" DESC");
            }
        }
        if let Some(n) = &self.limit {
            out.push_str(&format!(" LIMIT {n}"));
        }
        out
    }
}

/// A partial-pushdown plan: the leading row predicates as a WHERE
/// clause for `table`'s fetch. The caller runs the *original* query
/// against the filtered adapter — the engine re-applies the pushed
/// predicates (a no-op on pre-filtered rows), so no rewriting.
pub struct Partial {
    pub table: String,
    pub where_sql: String,
}

/// Attempt a partial pushdown: `Some` when the query's leading row
/// predicates translate strictly and the rest of the query provably
/// cannot observe the filtering. The gates, each with its reason:
/// a single `/table/*` branch with no correlations (other shapes
/// address other data); only the *leading* run of expression
/// predicates pushes (a positional predicate before an expression
/// one sees unfiltered rows on the scan path); the table's name
/// appears exactly once among the query's steps (a second reach —
/// `^`-anchored subcontexts, self-references — would see the
/// filtered subset); no crosslink or resolution axes anywhere
/// (backlinks and reverse resolution into the table would too);
/// and no `:::` / `;;;` metadata anywhere (a filtered `;;;n-rows`
/// would lie).
pub fn partial_pushdown(quarb: &str) -> Option<Partial> {
    partial_pushdown_explained(quarb).ok()
}

/// [`partial_pushdown`], keeping the refusal reason.
pub fn partial_pushdown_explained(quarb: &str) -> Result<Partial, SqlError> {
    refuse_marker(quarb)?;
    let arbor =
        QueryArbor::parse(quarb).map_err(|e| SqlError::Syntax(format!("parsing Quarb: {e}")))?;
    refuse_groups(&arbor)?;
    let mut ex = Exporter {
        arbor,
        notes: Vec::new(),
        strict: true,
        dialect: None,
        from_table: String::new(),
        join_on_left_cols: Vec::new(),
        join_table: None,
        aggregate: false,
    };
    ex.partial()
}

/// Refuse a query carrying path-pattern groups: neither the SQL
/// translation nor the pushdown safe set covers them, and the shape
/// checks below count only `step` children — an unguarded group
/// would silently vanish from the translation. Refused queries fall
/// back to the scan path, which evaluates groups correctly.
fn refuse_groups(arbor: &QueryArbor) -> Result<(), SqlError> {
    let mut stack = vec![arbor.root()];
    while let Some(n) = stack.pop() {
        if arbor.name(n).as_deref() == Some("group") {
            return Err(SqlError::Unsupported(
                "path patterns (groups and quantifiers)".into(),
            ));
        }
        stack.extend(arbor.children(n));
    }
    Ok(())
}

struct Exporter {
    arbor: QueryArbor,
    notes: Vec<String>,
    /// Pushdown mode: refuse every construct whose SQL semantics
    /// are not provably identical to Quarb's (LIKE case folding,
    /// truthiness, group/distinct/sort ordering).
    strict: bool,
    /// The target dialect for JSON-column-path pushdown; `None`
    /// leaves such filters on the client-side graft.
    dialect: Option<Dialect>,
    from_table: String,
    join_on_left_cols: Vec<String>,
    join_table: Option<String>,
    aggregate: bool,
}

impl Exporter {
    fn kids(&self, n: NodeId, kind: &str) -> Vec<NodeId> {
        self.arbor
            .children(n)
            .into_iter()
            .filter(|&c| self.arbor.name(c).as_deref() == Some(kind))
            .collect()
    }

    fn kid(&self, n: NodeId, kind: &str) -> Option<NodeId> {
        self.kids(n, kind).into_iter().next()
    }

    fn prop(&self, n: NodeId, key: &str) -> Option<Value> {
        self.arbor.property(n, key)
    }

    fn prop_s(&self, n: NodeId, key: &str) -> String {
        self.prop(n, key).map(|v| v.to_string()).unwrap_or_default()
    }

    fn kind(&self, n: NodeId) -> String {
        self.arbor.name(n).unwrap_or_default()
    }

    /// Whether `n` is a bare `null` literal operand.
    fn is_null_literal(&self, n: NodeId) -> bool {
        self.kind(n) == "literal" && self.prop_s(n, "type") == "null"
    }

    /// The partial-pushdown analysis (see [`partial_pushdown`]).
    fn partial(&mut self) -> Result<Partial, SqlError> {
        let root = self.arbor.root();
        let q = self
            .kid(root, "query")
            .ok_or_else(|| SqlError::Unsupported("empty query".into()))?;
        if self.kid(q, "query").is_some() {
            return Err(SqlError::Unsupported(
                "partial pushdown: correlations address other tables".into(),
            ));
        }
        let branches = self.kids(q, "branch");
        if branches.len() != 1 {
            return Err(SqlError::Unsupported(
                "partial pushdown: a branch union".into(),
            ));
        }
        let (table, preds) = self.table_branch(branches[0])?;

        // Whole-query gates.
        let all = self.walk_all(root);
        let mut table_mentions = 0;
        for n in &all {
            match self.kind(*n).as_str() {
                "step" => {
                    let axis = self.prop_s(*n, "axis");
                    if matches!(axis.as_str(), "->" | "<-" | "~>" | "<~") {
                        return Err(SqlError::Unsupported(
                            "partial pushdown: crosslink/resolution axes could reach \
                             the filtered table"
                                .into(),
                        ));
                    }
                    if self.prop_s(*n, "matcher") == table {
                        table_mentions += 1;
                    }
                }
                "projection" if self.prop_s(*n, "kind") != "property" => {
                    return Err(SqlError::Unsupported(
                        "partial pushdown: metadata would observe the filtering \
                         (;;;n-rows, :::index)"
                            .into(),
                    ));
                }
                _ => {}
            }
        }
        if table_mentions != 1 {
            return Err(SqlError::Unsupported(
                "partial pushdown: the table is reached more than once".into(),
            ));
        }

        // The leading run of expression predicates, strictly
        // translated.
        let mut conds = Vec::new();
        for p in preds {
            if self.prop_s(p, "kind") != "expr" {
                break;
            }
            conds.push(self.predicate_cond(p, None)?);
        }
        if conds.is_empty() {
            return Err(SqlError::Unsupported(
                "partial pushdown: no leading expression predicates to push".into(),
            ));
        }
        Ok(Partial {
            table,
            where_sql: conds.join(" AND "),
        })
    }

    fn walk_all(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = vec![n];
        let mut i = 0;
        while i < out.len() {
            out.extend(self.arbor.children(out[i]));
            i += 1;
        }
        out
    }

    fn query(&mut self) -> Result<String, SqlError> {
        let root = self.arbor.root();
        let q = self
            .kid(root, "query")
            .ok_or_else(|| SqlError::Unsupported("empty query".into()))?;
        let mut sel = Select::default();

        // Correlations: exactly one becomes the JOIN's left table.
        let corrs = self.kids(q, "query");
        let branches = self.kids(q, "branch");
        if branches.len() != 1 {
            return Err(SqlError::Unsupported(
                "a branch union (SQL has UNION, but result shapes differ; export one branch)"
                    .into(),
            ));
        }

        if corrs.len() > 1 {
            return Err(SqlError::Unsupported(
                "more than one correlation context".into(),
            ));
        }
        if let Some(corr) = corrs.first() {
            // Left table from the correlation context.
            let (ltable, lpreds) = self.table_branch(self.kids(*corr, "branch")[0])?;
            if !lpreds.is_empty() {
                return Err(SqlError::Unsupported(
                    "predicates on the correlation context (put them on the joined side)".into(),
                ));
            }
            let (rtable, rpreds) = self.table_branch(branches[0])?;
            // The SQL renderings of the two table names (validated
            // or quoted); the raw names stay in the plan metadata,
            // which the driver matches against its catalog.
            let lsql = self.sql_ident(&ltable, "table name")?;
            let rsql = self.sql_ident(&rtable, "table name")?;
            // Split the joined side's predicates: $*1 equalities
            // form the ON, the rest the WHERE.
            let mut on = Vec::new();
            let mut wheres = Vec::new();
            for p in rpreds {
                self.split_join_pred(p, &lsql, &rsql, &mut on, &mut wheres)?;
            }
            if on.is_empty() {
                return Err(SqlError::Unsupported(
                    "a correlation without a '$*1' equality (no JOIN condition)".into(),
                ));
            }
            self.notes.push(
                "JOIN: Quarb's binding is existential (one row per joined-side row); \
                 SQL multiplies rows when several left rows match"
                    .to_string(),
            );
            sel.from = lsql.clone();
            self.from_table = ltable;
            self.join_table = Some(rtable);
            sel.join = Some((rsql.clone(), on.join(" AND ")));
            sel.wheres = wheres;
            // A terminal projection on the joined branch is a
            // one-column select of the result context's table.
            if let Some(proj) = self.kid(branches[0], "projection") {
                let col = self.projection_col(proj)?;
                let col = self.sql_ident(&col, "column name")?;
                sel.select.push(format!("{rsql}.{col}"));
            }
            self.pipeline(q, &mut sel, Some((&lsql, &rsql)))?;
        } else {
            let (table, preds) = self.table_branch(branches[0])?;
            sel.from = self.sql_ident(&table, "table name")?;
            self.from_table = table;
            for p in preds {
                let cond = self.predicate_cond(p, None)?;
                sel.wheres.push(cond);
            }
            // A terminal projection is a one-column select.
            if let Some(proj) = self.kid(branches[0], "projection") {
                let col = self.projection_col(proj)?;
                sel.select.push(self.sql_ident(&col, "column name")?);
            }
            self.pipeline(q, &mut sel, None)?;
        }
        Ok(sel.render())
    }

    /// A `/table/*[preds]` branch: the table name and the row-step
    /// predicate nodes.
    fn table_branch(&mut self, b: NodeId) -> Result<(String, Vec<NodeId>), SqlError> {
        let steps = self.kids(b, "step");
        if steps.len() != 2 {
            return Err(SqlError::Unsupported(
                "navigation beyond /table/* (SQL sees tables and rows)".into(),
            ));
        }
        let (t, rows) = (steps[0], steps[1]);
        if self.prop_s(t, "axis") != "/"
            || self.prop_s(t, "matcher-kind") != "name"
            || self.prop_s(rows, "axis") != "/"
            || self.prop_s(rows, "matcher-kind") != "any"
        {
            return Err(SqlError::Unsupported(
                "navigation beyond /table/* (SQL sees tables and rows)".into(),
            ));
        }
        if self.kid(t, "predicate").is_some() {
            return Err(SqlError::Unsupported("a predicate on the table hop".into()));
        }
        Ok((self.prop_s(t, "matcher"), self.kids(rows, "predicate")))
    }

    /// One row predicate as a WHERE condition (qualify columns with
    /// `qualifier` when joining).
    fn predicate_cond(&mut self, p: NodeId, qual: Option<&str>) -> Result<String, SqlError> {
        if self.prop_s(p, "kind") != "expr" {
            return Err(SqlError::Unsupported(
                "a positional predicate on rows (SQL rows are unordered; ORDER BY + LIMIT)".into(),
            ));
        }
        let parts: Vec<String> = self
            .arbor
            .children(p)
            .into_iter()
            .map(|c| self.pred_expr(c, qual))
            .collect::<Result<_, _>>()?;
        Ok(parts.join(" AND "))
    }

    fn pred_expr(&mut self, e: NodeId, qual: Option<&str>) -> Result<String, SqlError> {
        match self.kind(e).as_str() {
            "and" | "or" => {
                let op = self.kind(e).to_uppercase();
                let kids: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c, qual))
                    .collect::<Result<_, _>>()?;
                Ok(format!("({})", kids.join(&format!(" {op} "))))
            }
            "not" => {
                // SQL's `NOT` propagates UNKNOWN: `NOT (x = 5)` is
                // UNKNOWN for a NULL `x` and drops the row, but Quarb's
                // negation keeps it (the inner `value_eq` is false, so
                // its negation is true). Not provably identical without
                // the schema — the pushdown paths refuse it and scan.
                if self.strict {
                    return Err(SqlError::Unsupported(
                        "pushdown: 'not(...)' drops the NULL rows Quarb keeps \
                         (SQL NOT propagates UNKNOWN)"
                            .into(),
                    ));
                }
                let inner: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c, qual))
                    .collect::<Result<_, _>>()?;
                Ok(format!("NOT ({})", inner.join(" AND ")))
            }
            "parens" => {
                let inner: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c, qual))
                    .collect::<Result<_, _>>()?;
                Ok(format!("({})", inner.join(" AND ")))
            }
            "compare" => {
                let op = self.prop_s(e, "op");
                let kids = self.arbor.children(e);
                // A comparison against a bare `null` literal. Quarb's
                // `value_eq` treats NULL as an ordinary value
                // (`value_eq(NULL, NULL)` is true, `value_eq(NULL, x)`
                // false), so `= null` keeps exactly the NULL rows and
                // `!= null` the non-NULL rows. SQL's `= NULL` / `<>
                // NULL` are always UNKNOWN and drop every row; the
                // `IS [NOT] NULL` forms are provably identical (and
                // portable across every target dialect).
                if matches!(op.as_str(), "=" | "!=")
                    && (self.is_null_literal(kids[0]) || self.is_null_literal(kids[1]))
                {
                    let other = if self.is_null_literal(kids[0]) {
                        kids[1]
                    } else {
                        kids[0]
                    };
                    let col = self.operand(other, qual)?;
                    return Ok(if op == "=" {
                        format!("{col} IS NULL")
                    } else {
                        format!("{col} IS NOT NULL")
                    });
                }
                // JSON-column-path pushdown: `[/col/a/b:: = 'lit']`
                // navigates into a JSON column and compares a fixed
                // path to a string literal. Only this exact shape —
                // fixed object path, string equality — is provably
                // identical to the client-side graft (each engine's
                // scalar extractor unquotes to text, and an absent
                // path or non-string value excludes the row on both
                // sides, matching Quarb's `value_eq`). Numeric casts,
                // `!=`, wildcards, and deeper predicates are *not*
                // handled here, so they fall through to the ordinary
                // operand logic below, which refuses the navigation
                // and scans. Enabled only when a dialect is set.
                if op == "="
                    && let Some(dialect) = self.dialect
                {
                    for (pi, li) in [(0usize, 1usize), (1, 0)] {
                        if self.is_text_literal(kids[li])
                            && let Some((col, path)) = self.json_path(kids[pi])
                        {
                            let lit = self.operand(kids[li], qual)?;
                            let extract = json_extract(dialect, qual, &col, &path);
                            return Ok(format!("{extract} = {lit}"));
                        }
                    }
                }
                let l = self.operand(kids[0], qual)?;
                let r = self.operand(kids[1], qual)?;
                Ok(match op.as_str() {
                    "=" => format!("{l} = {r}"),
                    "!=" => {
                        // Quarb keeps rows whose operand is NULL (its
                        // `value_eq` is false there, so `!=` is true);
                        // SQL's `<>` is UNKNOWN for a NULL operand and
                        // drops those rows. Not provably identical
                        // without the schema, so pushdown refuses it;
                        // the display translation keeps `<>` and notes
                        // the divergence.
                        if self.strict {
                            return Err(SqlError::Unsupported(
                                "pushdown: '!=' drops the NULL rows Quarb keeps \
                                 (SQL '<>' is UNKNOWN for NULL; use '!= null' \
                                 for IS NOT NULL)"
                                    .into(),
                            ));
                        }
                        self.notes.push(
                            "'!=' → '<>': Quarb keeps rows whose column is NULL; \
                             SQL's '<>' drops them (use '!= null' for IS NOT NULL)"
                                .to_string(),
                        );
                        format!("{l} <> {r}")
                    }
                    "<" | "<=" | ">" | ">=" => format!("{l} {op} {r}"),
                    "*=" => {
                        if self.strict {
                            return Err(SqlError::Unsupported(
                                "pushdown: LIKE case folding differs per engine".into(),
                            ));
                        }
                        // The pattern must be a text literal: a
                        // column or computed operand holds a value,
                        // not a pattern, and SQL LIKE cannot express
                        // "contains that value" portably.
                        if !self.is_text_literal(kids[1]) {
                            return Err(SqlError::Unsupported(
                                "'*=' with a non-literal pattern".into(),
                            ));
                        }
                        self.notes.push(
                            "*= → LIKE: Quarb's substring test is case-sensitive; LIKE \
                             folds case on SQLite/MySQL but not PostgreSQL"
                                .to_string(),
                        );
                        // Escape LIKE's metacharacters, then quote.
                        // The explicit ESCAPE makes '\' the escape
                        // everywhere — SQLite, MSSQL, and Oracle
                        // have no default escape character.
                        let raw = self
                            .prop(kids[1], "value")
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        let pat = raw
                            .replace('\\', "\\\\")
                            .replace('%', "\\%")
                            .replace('_', "\\_")
                            .replace('\'', "''");
                        format!("{l} LIKE '%{pat}%' ESCAPE '\\'")
                    }
                    "=~" | "!~" => {
                        return Err(SqlError::Unsupported(
                            "regex matching (REGEXP dialects disagree; use *= or spell \
                             the SQL by hand)"
                                .into(),
                        ));
                    }
                    other => {
                        return Err(SqlError::Unsupported(format!("the '{other}' comparison")));
                    }
                })
            }
            // A bare truthy operand.
            _ => {
                if self.strict {
                    return Err(SqlError::Unsupported(
                        "pushdown: truthiness diverges (0 and '' are falsy in Quarb)".into(),
                    ));
                }
                self.notes.push(
                    "truthiness: '[::c]' exports as IS NOT NULL, but Quarb also treats \
                     0 and '' as falsy"
                        .to_string(),
                );
                Ok(format!("{} IS NOT NULL", self.operand(e, qual)?))
            }
        }
    }

    fn operand(&mut self, o: NodeId, qual: Option<&str>) -> Result<String, SqlError> {
        match self.kind(o).as_str() {
            "literal" => {
                let v = self.prop(o, "value").unwrap_or(Value::Null);
                match self.prop_s(o, "type").as_str() {
                    "text" => {
                        let s = v.to_string();
                        // No single escaping is portable across the
                        // pushdown's target dialects: MySQL (default
                        // sql_mode) and BigQuery read `\` as an escape
                        // while SQLite/PostgreSQL/DuckDB take it
                        // literally, and BigQuery rejects the `''`
                        // quote-doubling the others require. A literal
                        // carrying either character cannot be pushed as
                        // provably identical SQL, so refuse it and let
                        // the caller scan. (The display translation
                        // keeps its best-effort `''`-doubling.)
                        if self.strict && (s.contains('\'') || s.contains('\\')) {
                            return Err(SqlError::Unsupported(
                                "pushdown: a text literal with a quote or backslash \
                                 has no escaping portable across SQL dialects"
                                    .into(),
                            ));
                        }
                        Ok(format!("'{}'", s.replace('\'', "''")))
                    }
                    "null" => Ok("NULL".to_string()),
                    _ => Ok(v.to_string()),
                }
            }
            "path" => {
                if self.kid(o, "step").is_some() {
                    // A step here is either navigation (refused) or
                    // a resolution chain (refused with the reason).
                    let s = self.kids(o, "step")[0];
                    if self.prop_s(s, "axis") == "~>" {
                        return Err(SqlError::Unsupported(
                            "a '~>' resolution chain: the foreign-key targets live in \
                             the schema, not the query — spell the join with '<=>' to \
                             export it"
                                .into(),
                        ));
                    }
                    return Err(SqlError::Unsupported(
                        "navigation inside a predicate (SQL rows are flat)".into(),
                    ));
                }
                let p = self
                    .kid(o, "projection")
                    .ok_or_else(|| SqlError::Unsupported("an empty path operand".into()))?;
                let col = self.projection_col(p)?;
                let col = self.sql_ident(&col, "column name")?;
                Ok(match qual {
                    Some(q) => format!("{q}.{col}"),
                    None => col,
                })
            }
            "context" => {
                // `$*k::col` — k names the correlation operand: 1 is
                // the left/FROM side, 2 the joined side (the `qual`
                // table in a join context). Anything else is outside
                // the verified-safe set. Outside a join (`qual` is
                // None) there is no table for either to name, so
                // refuse — the `__LEFT__` placeholder would leak
                // into the emitted SQL unsubstituted.
                let p = self
                    .kid(o, "projection")
                    .ok_or_else(|| SqlError::Unsupported("a bare '$*' reference".into()))?;
                let col = self.projection_col(p)?;
                let col = self.sql_ident(&col, "column name")?;
                if qual.is_none() {
                    return Err(SqlError::Unsupported(
                        "a '$*' reference outside a correlation join".into(),
                    ));
                }
                match self.prop(o, "index") {
                    None | Some(Value::Int(1)) => Ok(format!("__LEFT__.{col}")),
                    Some(Value::Int(2)) => Ok(format!("{}.{col}", qual.expect("checked above"))),
                    Some(v) => Err(SqlError::Unsupported(format!(
                        "pushdown: $*{v} beyond a two-branch correlation"
                    ))),
                }
            }
            "arith" => {
                let op = self.prop_s(o, "op");
                let kids = self.arbor.children(o);
                let l = self.operand(kids[0], qual)?;
                let r = self.operand(kids[1], qual)?;
                Ok(match op.as_str() {
                    "+" | "-" | "*" => format!("({l} {op} {r})"),
                    "div" => format!("({l} / {r})"),
                    "mod" => format!("({l} % {r})"),
                    other => return Err(SqlError::Unsupported(format!("'{other}' arithmetic"))),
                })
            }
            other => Err(SqlError::Unsupported(format!(
                "the '{other}' operand (registers, topics, and captures are Quarb-side state)"
            ))),
        }
    }

    fn projection_col(&mut self, p: NodeId) -> Result<String, SqlError> {
        match self.prop_s(p, "kind").as_str() {
            "property" => match self.prop(p, "key") {
                Some(k) => Ok(k.to_string()),
                None => Err(SqlError::Unsupported(
                    "the bare '::' projection (name the column)".into(),
                )),
            },
            other => Err(SqlError::Unsupported(format!(
                "the {other} metadata projection"
            ))),
        }
    }

    /// Whether `o` is a text string literal (`'London'`).
    fn is_text_literal(&self, o: NodeId) -> bool {
        self.kind(o) == "literal" && self.prop_s(o, "type") == "text"
    }

    /// If `o` is the one JSON-column-path shape pushdown handles —
    /// `/col/seg/seg…::`, a plain-navigation path (every hop `/`
    /// and a plain-identifier object key, no wildcards, no nested
    /// predicate), at least one segment past the column, ending in
    /// the bare `::` projection — return `(column, [json segments])`.
    /// Anything else is `None` (and falls back to the graft).
    fn json_path(&self, o: NodeId) -> Option<(String, Vec<String>)> {
        if self.kind(o) != "path" {
            return None;
        }
        let steps = self.kids(o, "step");
        if steps.len() < 2 {
            return None;
        }
        // The projection must be the bare `::` (default value of
        // the JSON leaf), not `::key` or a `;;;`/`:::` metadata form.
        let proj = self.kid(o, "projection")?;
        if self.prop_s(proj, "kind") != "property" || self.prop(proj, "key").is_some() {
            return None;
        }
        let mut names = Vec::with_capacity(steps.len());
        for s in &steps {
            if self.prop_s(*s, "axis") != "/"
                || self.prop_s(*s, "matcher-kind") != "name"
                || self.kid(*s, "predicate").is_some()
            {
                return None;
            }
            let name = self.prop_s(*s, "matcher");
            // Plain object keys only — no array indices, no
            // characters that would need per-dialect path escaping.
            let plain = !name.is_empty()
                && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !plain {
                return None;
            }
            names.push(name);
        }
        let col = names.remove(0);
        Some((col, names))
    }

    /// A safe `AS` alias: bare when it is a plain identifier and
    /// not an SQL keyword; otherwise strict mode refuses (quoting
    /// dialects disagree) and export mode double-quotes, noting it.
    fn alias(&mut self, name: &str) -> Result<String, SqlError> {
        if is_plain_ident(name) {
            return Ok(name.to_string());
        }
        if self.strict {
            return Err(SqlError::Unsupported(format!(
                "pushdown: field name {name:?} needs SQL quoting \
                 (dialects disagree); rename the field"
            )));
        }
        self.notes
            .push(format!("field name {name:?} double-quoted (ANSI)"));
        Ok(format!("\"{}\"", name.replace('"', "\"\"")))
    }

    /// A table or column name rendered into SQL. Bare when it is a
    /// plain identifier; otherwise strict mode refuses — a name
    /// like `a OR b` would silently rewrite the emitted SQL's
    /// meaning, breaking the provably-identical guarantee (and a
    /// portable quoting does not exist) — and export mode
    /// double-quotes with a note.
    fn sql_ident(&mut self, name: &str, what: &str) -> Result<String, SqlError> {
        if is_plain_ident(name) {
            return Ok(name.to_string());
        }
        if self.strict {
            return Err(SqlError::Unsupported(format!(
                "pushdown: {what} {name:?} is not a plain SQL identifier"
            )));
        }
        self.notes
            .push(format!("{what} {name:?} double-quoted (ANSI)"));
        Ok(format!("\"{}\"", name.replace('"', "\"\"")))
    }

    /// Split a joined-side predicate: `col = $*1::col2` equalities
    /// become the ON condition; everything else the WHERE.
    /// `ltable` and `rtable` are the tables' SQL renderings.
    fn split_join_pred(
        &mut self,
        p: NodeId,
        ltable: &str,
        rtable: &str,
        on: &mut Vec<String>,
        wheres: &mut Vec<String>,
    ) -> Result<(), SqlError> {
        if self.prop_s(p, "kind") != "expr" {
            return Err(SqlError::Unsupported(
                "a positional predicate on the joined side".into(),
            ));
        }
        // Flatten top-level ANDs; each conjunct routes to ON or
        // WHERE.
        fn conjuncts(ex: &Exporter, e: NodeId, out: &mut Vec<NodeId>) {
            if ex.kind(e) == "and" {
                for c in ex.arbor.children(e) {
                    conjuncts(ex, c, out);
                }
            } else {
                out.push(e);
            }
        }
        let mut parts = Vec::new();
        for c in self.arbor.children(p) {
            conjuncts(self, c, &mut parts);
        }
        for e in parts {
            let uses_ctx = self.subtree_has(e, "context");
            let cond = self.pred_expr(e, Some(rtable))?.replace("__LEFT__", ltable);
            if uses_ctx && self.kind(e) == "compare" && self.prop_s(e, "op") == "=" {
                // Record which left-table columns the ON binds —
                // the driver's uniqueness obligation (see
                // Pushdown::join_left). Collected from the arbor's
                // `$*1` operand nodes, never from the rendered SQL
                // text, so neither a literal nor an unusual column
                // name can corrupt the obligation.
                self.collect_left_cols(e)?;
                on.push(cond);
            } else {
                wheres.push(cond);
            }
        }
        Ok(())
    }

    /// The left-table columns a `$*1::col` operand binds, anywhere
    /// in `e`'s subtree, appended to the join obligation with their
    /// raw (catalog) names.
    fn collect_left_cols(&mut self, e: NodeId) -> Result<(), SqlError> {
        if self.kind(e) == "context"
            && matches!(self.prop(e, "index"), None | Some(Value::Int(1)))
            && let Some(p) = self.kid(e, "projection")
        {
            let col = self.projection_col(p)?;
            self.join_on_left_cols.push(col);
        }
        for c in self.arbor.children(e) {
            self.collect_left_cols(c)?;
        }
        Ok(())
    }

    fn subtree_has(&self, n: NodeId, kind: &str) -> bool {
        if self.kind(n) == kind {
            return true;
        }
        self.arbor
            .children(n)
            .into_iter()
            .any(|c| self.subtree_has(c, kind))
    }

    /// Consume the pipeline into SELECT clauses.
    fn pipeline(
        &mut self,
        q: NodeId,
        sel: &mut Select,
        join: Option<(&str, &str)>,
    ) -> Result<(), SqlError> {
        let Some(pipe) = self.kid(q, "pipeline") else {
            return Ok(());
        };
        let stages: Vec<NodeId> = self.arbor.children(pipe);
        let mut i = 0;
        // A pending per-row value (`| ::col`) feeding an aggregate.
        let mut pending_col: Option<String> = None;
        // After a grouped reduction: (key, agg_sql, alias).
        let mut grouped: Option<(String, String, String)> = None;

        while i < stages.len() {
            let s = stages[i];
            match self.kind(s).as_str() {
                "expr" => {
                    let kids = self.arbor.children(s);
                    pending_col = Some(self.operand(kids[0], join.map(|(_, r)| r))?);
                }
                "func" => {
                    let name = self.prop_s(s, "name");
                    match name.as_str() {
                        "rec" | "record" => {
                            sel.select = self.record_fields(s, join)?;
                        }
                        // A reducing aggregate on the plain pipe:
                        // the grouped reduction.
                        "count" | "sum" | "mean" | "avg" | "min" | "max" => {
                            let (key, _, _) = grouped.as_ref().ok_or_else(|| {
                                SqlError::Unsupported(format!(
                                    "'| {name}' outside a group (use '@| {name}')"
                                ))
                            })?;
                            // Quarb's count counts every member,
                            // nulls included: COUNT(*), never the
                            // NULL-skipping COUNT(col).
                            let col = if name == "count" {
                                pending_col = None;
                                None
                            } else {
                                pending_col.take()
                            };
                            let agg = sql_agg(&name, col)?;
                            grouped = Some((key.clone(), agg.clone(), agg));
                        }
                        other => {
                            return Err(SqlError::Unsupported(format!(
                                "the '{other}' pipeline function"
                            )));
                        }
                    }
                }
                "push" => {
                    // An alias for the grouped aggregate.
                    if let Some((k, agg, _)) = grouped.take() {
                        let alias = self.prop_s(s, "name");
                        grouped = Some((k, agg, alias));
                    } else {
                        return Err(SqlError::Unsupported(
                            "a register push (Quarb-side state)".into(),
                        ));
                    }
                }
                "filter" => {
                    if grouped.is_some() {
                        // HAVING: `$_` / the alias refer to the
                        // aggregate.
                        let cond = self.having_cond(s)?;
                        sel.having = Some(cond);
                    } else {
                        return Err(SqlError::Unsupported(
                            "a mid-pipeline filter (put it in the row predicate)".into(),
                        ));
                    }
                }
                "recall" => {
                    // `| %.` finalizes the grouped record.
                    if self.prop_s(s, "ref") != "%." {
                        return Err(SqlError::Unsupported(
                            "a register recall (Quarb-side state)".into(),
                        ));
                    }
                    let (key, agg, alias) = grouped.clone().ok_or_else(|| {
                        SqlError::Unsupported("'%.', with nothing grouped".into())
                    })?;
                    let alias = self.alias(&alias)?;
                    sel.select = vec![key.clone(), format!("{agg} AS {alias}")];
                    // The HAVING condition compared the aggregate
                    // through $_ — substitute the real expression.
                    if let Some(h) = sel.having.take() {
                        sel.having = Some(h.replace("__AGG__", &agg));
                    }
                }
                "agg" => {
                    let name = self.prop_s(s, "name");
                    match name.as_str() {
                        "count" | "sum" | "mean" | "avg" | "min" | "max" => {
                            // Quarb's count counts every row, nulls
                            // included: COUNT(*) regardless of a
                            // pending column.
                            let col = if name == "count" {
                                pending_col = None;
                                None
                            } else {
                                pending_col.take()
                            };
                            sel.select = vec![sql_agg(&name, col)?];
                            self.aggregate = true;
                        }
                        "group" => {
                            if self.strict {
                                return Err(SqlError::Unsupported(
                                    "pushdown: GROUP BY result order is unordered in SQL".into(),
                                ));
                            }
                            self.notes.push(
                                "GROUP BY: SQL keeps a NULL-key group; Quarb's group \
                                 drops null keys"
                                    .to_string(),
                            );
                            let key = self.group_key(s, join)?;
                            sel.group_by = Some(key.clone());
                            grouped = Some((key, String::new(), String::new()));
                        }
                        "sort_by" => {
                            if self.strict {
                                return Err(SqlError::Unsupported(
                                    "pushdown: ORDER BY collations differ per engine".into(),
                                ));
                            }
                            // A sort after a positional selection
                            // (or a second sort) cannot render: the
                            // fixed SELECT shape orders before
                            // LIMIT, and has one ORDER BY.
                            if sel.limit.is_some() {
                                return Err(SqlError::Unsupported(
                                    "a sort after a positional selection (SQL orders \
                                     before LIMIT)"
                                        .into(),
                                ));
                            }
                            if sel.order_by.is_some() {
                                return Err(SqlError::Unsupported(
                                    "a second sort (SQL has a single ORDER BY)".into(),
                                ));
                            }
                            let kids = self.arbor.children(s);
                            let e = self.operand(kids[0], join.map(|(_, r)| r))?;
                            sel.order_by = Some((e, false));
                        }
                        "reverse" => match &mut sel.order_by {
                            Some((_, desc)) => *desc = true,
                            None => {
                                return Err(SqlError::Unsupported(
                                    "reverse without an ORDER BY (rows are unordered)".into(),
                                ));
                            }
                        },
                        "top" => {
                            if self.strict {
                                return Err(SqlError::Unsupported(
                                    "pushdown: ORDER BY collations differ per engine".into(),
                                ));
                            }
                            if sel.limit.is_some() {
                                return Err(SqlError::Unsupported(
                                    "'top' after a positional selection (SQL orders \
                                     before LIMIT)"
                                        .into(),
                                ));
                            }
                            if sel.order_by.is_some() {
                                return Err(SqlError::Unsupported(
                                    "a second sort (SQL has a single ORDER BY)".into(),
                                ));
                            }
                            let kids = self.arbor.children(s);
                            let n = self.prop_s(kids[0], "value");
                            let e = self.operand(kids[1], join.map(|(_, r)| r))?;
                            sel.order_by = Some((e, true));
                            sel.limit = Some(n);
                        }
                        "unique" => {
                            if self.strict {
                                return Err(SqlError::Unsupported(
                                    "pushdown: DISTINCT result order is unordered in SQL".into(),
                                ));
                            }
                            // Quarb dedups the limited rows; SQL
                            // applies DISTINCT before LIMIT.
                            if sel.limit.is_some() {
                                return Err(SqlError::Unsupported(
                                    "'unique' after a positional selection (SQL applies \
                                     DISTINCT before LIMIT)"
                                        .into(),
                                ));
                            }
                            if let Some(c) = pending_col.take() {
                                sel.select = vec![c];
                            }
                            sel.distinct = true;
                        }
                        other => {
                            return Err(SqlError::Unsupported(format!("the '{other}' aggregate")));
                        }
                    }
                }
                "select" => {
                    if self.strict {
                        return Err(SqlError::Unsupported(
                            "pushdown: LIMIT without a guaranteed order".into(),
                        ));
                    }
                    // `@| [..n]` → LIMIT.
                    if sel.limit.is_some() {
                        return Err(SqlError::Unsupported(
                            "a second positional selection".into(),
                        ));
                    }
                    let p = self.arbor.children(s)[0];
                    match (self.prop_s(p, "kind").as_str(), self.prop(p, "to")) {
                        ("range", Some(Value::Int(n)))
                            if self.prop(p, "from").is_none() && n > 0 =>
                        {
                            sel.limit = Some(n.to_string());
                        }
                        _ => {
                            return Err(SqlError::Unsupported(
                                "positional selection beyond '@| [..n]'".into(),
                            ));
                        }
                    }
                }
                other => {
                    return Err(SqlError::Unsupported(format!(
                        "the '{other}' stage (windows, subcontexts, and registers are \
                         Quarb-side state)"
                    )));
                }
            }
            i += 1;
        }
        // A pending column with no aggregate is a one-column select.
        if let Some(c) = pending_col
            && sel.select.is_empty()
        {
            sel.select = vec![c];
        }
        Ok(())
    }

    fn group_key(&mut self, s: NodeId, join: Option<(&str, &str)>) -> Result<String, SqlError> {
        let kids = self.arbor.children(s);
        // group(::k) or group("name", expr) — the key expression is
        // the last child; a literal first child is its name.
        let key = kids
            .iter()
            .rev()
            .find(|&&k| self.kind(k) != "literal")
            .ok_or_else(|| SqlError::Unsupported("a literal group key".into()))?;
        self.operand(*key, join.map(|(_, r)| r))
    }

    /// A HAVING filter: `$_` and `$.name` refer to the aggregate.
    fn having_cond(&mut self, s: NodeId) -> Result<String, SqlError> {
        let kids = self.arbor.children(s);
        if kids.len() != 1 || self.kind(kids[0]) != "compare" {
            return Err(SqlError::Unsupported(
                "HAVING translates for a single comparison".into(),
            ));
        }
        let e = kids[0];
        let op = self.prop_s(e, "op");
        let cmp_kids = self.arbor.children(e);
        let l = match self.kind(cmp_kids[0]).as_str() {
            "topic" | "recall" => "__AGG__".to_string(),
            _ => {
                return Err(SqlError::Unsupported(
                    "HAVING compares the aggregate ($_ or its register)".into(),
                ));
            }
        };
        let r = self.operand(cmp_kids[1], None)?;
        Ok(format!("{l} {op} {r}"))
    }

    /// `rec(...)` fields as a select list.
    fn record_fields(
        &mut self,
        s: NodeId,
        join: Option<(&str, &str)>,
    ) -> Result<Vec<String>, SqlError> {
        let kids = self.arbor.children(s);
        let mut fields = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let k = kids[i];
            if self.kind(k) == "literal" {
                let name = self.prop_s(k, "value");
                let value = self.operand(kids[i + 1], join.map(|(_, r)| r))?;
                let value = match join {
                    Some((l, _)) => value.replace("__LEFT__", l),
                    None => value,
                };
                let name = self.alias(&name)?;
                fields.push(format!("{value} AS {name}"));
                i += 2;
            } else {
                let value = self.operand(k, join.map(|(_, r)| r))?;
                let value = match join {
                    Some((l, _)) => value.replace("__LEFT__", l),
                    None => value,
                };
                fields.push(value);
                i += 1;
            }
        }
        Ok(fields)
    }
}

fn sql_agg(name: &str, col: Option<String>) -> Result<String, SqlError> {
    let f = match name {
        "count" => "COUNT",
        "sum" => "SUM",
        "mean" | "avg" => "AVG",
        "min" => "MIN",
        "max" => "MAX",
        _ => unreachable!("checked by caller"),
    };
    match col {
        Some(c) => Ok(format!("{f}({c})")),
        // Only COUNT aggregates bare rows; SUM(*) and friends are
        // not SQL.
        None if f == "COUNT" => Ok("COUNT(*)".to_string()),
        None => Err(SqlError::Unsupported(format!(
            "'{name}' over row nodes (project a column first: '| ::col @| {name}')"
        ))),
    }
}

#[cfg(test)]
mod null_and_literal_tests {
    use super::{Dialect, export, partial_pushdown, pushdown};

    // Grouped pipeline that never pushes, so `partial_pushdown` hinges
    // only on the leading predicate (mirrors the crate's partial gate).
    const GROUPED: &str = " | ::x @| group(\"g\", ::x) | count | .n | %.";

    #[test]
    fn json_column_path_pushdown_per_dialect() {
        // `[/col/a/b:: = 'lit']` navigates into a JSON column; each
        // dialect extracts the fixed path to text and compares.
        // Verified live against all five engines to match the graft.
        let q = "/orders/*[/data/meta/tier:: = 'gold']::id";
        let sql = |d| pushdown(q, Some(d)).unwrap().sql;
        assert_eq!(
            sql(Dialect::Postgres),
            "SELECT id FROM orders WHERE (data::jsonb #>> '{meta,tier}') = 'gold'"
        );
        assert_eq!(
            sql(Dialect::MySql),
            "SELECT id FROM orders WHERE JSON_UNQUOTE(JSON_EXTRACT(data, '$.meta.tier')) = 'gold'"
        );
        assert_eq!(
            sql(Dialect::Sqlite),
            "SELECT id FROM orders WHERE json_extract(data, '$.meta.tier') = 'gold'"
        );
        assert_eq!(
            sql(Dialect::Mssql),
            "SELECT id FROM orders WHERE JSON_VALUE(data, '$.meta.tier') = 'gold'"
        );
        assert_eq!(
            sql(Dialect::Oracle),
            "SELECT id FROM orders WHERE JSON_VALUE(data, '$.meta.tier') = 'gold'"
        );
        // With no dialect, the JSON navigation is not pushable — it
        // falls back to the client-side graft.
        assert!(pushdown(q, None).is_none());
    }

    #[test]
    fn json_pushdown_only_string_equality() {
        // Numeric comparison, `!=`, and a wildcard hop stay off the
        // pushdown path (they are not provably identical to the
        // graft), so they refuse and scan even with a dialect set.
        let d = Some(Dialect::Sqlite);
        assert!(pushdown("/o/*[/data/n:: > 2]::id", d).is_none());
        assert!(pushdown("/o/*[/data/tier:: != 'gold']::id", d).is_none());
        assert!(pushdown("/o/*[/data/items/*/sku:: = 'A1']::id", d).is_none());
    }

    #[test]
    fn null_literal_compares_use_is_null() {
        // `= null` / `!= null` are provably identical to Quarb's
        // value_eq(NULL, …) semantics, never the always-UNKNOWN
        // `x = NULL`; they translate — and push — in both modes.
        assert_eq!(
            export("/t/*[::x = null] | ::x").unwrap().query,
            "SELECT x FROM t WHERE x IS NULL"
        );
        assert_eq!(
            export("/t/*[::x != null] | ::x").unwrap().query,
            "SELECT x FROM t WHERE x IS NOT NULL"
        );
        assert_eq!(
            pushdown("/t/*[::x = null] | ::x", None).unwrap().sql,
            "SELECT x FROM t WHERE x IS NULL"
        );
        assert_eq!(
            pushdown("/t/*[::x != null] | ::x", None).unwrap().sql,
            "SELECT x FROM t WHERE x IS NOT NULL"
        );
    }

    #[test]
    fn ne_and_not_refuse_pushdown_but_display_diverges() {
        // `!=` against a non-null value and `not(...)` drop the NULL
        // rows Quarb keeps under SQL three-valued logic: the pushdown
        // paths refuse (and scan), full and partial alike.
        assert!(pushdown("/t/*[::x != 5] | ::x", None).is_none());
        assert!(pushdown("/t/*[!::x = 5] | ::x", None).is_none());
        assert!(partial_pushdown(&format!("/t/*[::x != 5]{GROUPED}")).is_none());
        assert!(partial_pushdown(&format!("/t/*[!::x = 5]{GROUPED}")).is_none());
        // The display translation still emits `<>`, flagged with a note.
        let t = export("/t/*[::x != 5] | ::x").unwrap();
        assert_eq!(t.query, "SELECT x FROM t WHERE x <> 5");
        assert!(t.notes.iter().any(|n| n.contains("NULL")));
    }

    #[test]
    fn unescapable_text_literal_refuses_pushdown() {
        // A backslash (MySQL/BigQuery escape) or an apostrophe
        // (BigQuery rejects '' doubling) has no portable escaping, so
        // pushdown refuses; a clean literal still pushes.
        assert!(pushdown("/files/*[::path = \"C:\\temp\"] | ::path", None).is_none());
        assert!(pushdown("/t/*[::name = \"it's\"] | ::name", None).is_none());
        assert!(partial_pushdown(&format!("/t/*[::name = \"it's\"]{GROUPED}")).is_none());
        assert_eq!(
            pushdown("/t/*[::name = \"rare\"] | ::name", None).unwrap().sql,
            "SELECT name FROM t WHERE name = 'rare'"
        );
    }
}
