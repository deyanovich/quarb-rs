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
        join_on_cols: Vec::new(),
        join_table: None,
        from_sql: String::new(),
        join_sql: None,
        in_on: false,
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
    /// Present when the plan contains a witness JOIN: the joined
    /// table and the columns its ON equalities bind on it
    /// (collected structurally from the arbor, so query text
    /// cannot spoof them).
    /// The plan is only sound if those columns form a unique key
    /// of the joined table — each FROM row must find at most one
    /// witness, else SQL multiplies rows where Quarb's existential
    /// binding does not. The *driver* must verify against its
    /// catalog before executing, and fall back to the scan if it
    /// cannot.
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
        join_on_cols: Vec::new(),
        join_table: None,
        from_sql: String::new(),
        join_sql: None,
        in_on: false,
        aggregate: false,
    };
    let sql = ex.query()?;
    let order_table = if ex.aggregate {
        None
    } else {
        // Rows come back in the driver's document order — the FROM
        // table's (driver-first correlation).
        Some(ex.from_table.clone())
    };
    let join_left = ex
        .join_table
        .clone()
        .map(|t| (t, ex.join_on_cols.clone()));
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

/// Wrap a JSON-path condition in the dialect's validity guard,
/// where a cheap one exists. SQLite and MySQL error the whole
/// statement on one malformed-JSON row — which turned every
/// pushdown over a dirty table into a silent fallback scan — and
/// the graft excludes such rows anyway (no parse, no subtree), so
/// the guard is observationally identical. SQL Server gets ISJSON;
/// Oracle's JSON_VALUE is lax already; PostgreSQL has no guard
/// short of the ::jsonb cast itself (an invalid row errors and the
/// caller falls back to the scan).
fn json_valid_guard(dialect: Dialect, qual: Option<&str>, col: &str, cond: String) -> String {
    let qcol = match qual {
        Some(q) => format!("{q}.{col}"),
        None => col.to_string(),
    };
    match dialect {
        Dialect::Sqlite => format!("(json_valid({qcol}) AND {cond})"),
        Dialect::MySql => format!("(JSON_VALID({qcol}) AND {cond})"),
        Dialect::Mssql => format!("(ISJSON({qcol}) = 1 AND {cond})"),
        Dialect::Postgres | Dialect::Oracle => format!("({cond})"),
    }
}

/// The dialect's JSON type probe at a fixed path — non-NULL iff
/// the path exists. A JSON `null` value reports its type ('null'),
/// so existence still sees it, matching the graft, where a
/// null-valued key is a node. Only the dialects with a native
/// probe participate.
fn json_type_probe(
    dialect: Dialect,
    qual: Option<&str>,
    col: &str,
    path: &[String],
) -> Option<String> {
    let qcol = match qual {
        Some(q) => format!("{q}.{col}"),
        None => col.to_string(),
    };
    match dialect {
        Dialect::Sqlite => Some(format!("json_type({qcol}, '$.{}')", path.join("."))),
        Dialect::Postgres => Some(format!(
            "jsonb_typeof({qcol}::jsonb #> '{{{}}}')",
            path.join(",")
        )),
        Dialect::MySql | Dialect::Mssql | Dialect::Oracle => None,
    }
}

/// Flip a comparison for a swapped operand order (`lit OP path` →
/// `path OP' lit`).
fn flip_op(op: &str) -> String {
    match op {
        "<" => ">".into(),
        "<=" => ">=".into(),
        ">" => "<".into(),
        ">=" => "<=".into(),
        other => other.into(),
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
pub fn partial_pushdown(quarb: &str, dialect: Option<Dialect>) -> Option<Partial> {
    partial_pushdown_explained(quarb, dialect).ok()
}

/// [`partial_pushdown`], keeping the refusal reason. `dialect`
/// enables the JSON-column prefilters — the strict fixed-path
/// string equality, plus the relaxed superset forms (numeric
/// comparisons, existence) that are safe here and only here,
/// because the caller re-runs the original query over the
/// fetched subset.
pub fn partial_pushdown_explained(
    quarb: &str,
    dialect: Option<Dialect>,
) -> Result<Partial, SqlError> {
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
        join_on_cols: Vec::new(),
        join_table: None,
        from_sql: String::new(),
        join_sql: None,
        in_on: false,
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
    join_on_cols: Vec<String>,
    join_table: Option<String>,
    from_sql: String,
    join_sql: Option<String>,
    in_on: bool,
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
                "correlations address other tables".into(),
            ));
        }
        let branches = self.kids(q, "branch");
        if branches.len() != 1 {
            return Err(SqlError::Unsupported(
                "a branch union".into(),
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
                    if matches!(axis.as_str(), "->" | "<-" | "--" | "-->" | "<--") {
                        return Err(SqlError::Unsupported(
                            "crosslink/resolution axes could reach \
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
                        "metadata would observe the filtering \
                         (;;;n-rows, :::index)"
                            .into(),
                    ));
                }
                _ => {}
            }
        }
        if table_mentions != 1 {
            return Err(SqlError::Unsupported(
                "the table is reached more than once".into(),
            ));
        }

        // The leading run of expression predicates. Strict
        // translation first; a predicate the strict translator
        // refuses tries the relaxed prefilter ladder — superset
        // semantics: the fetched set may hold extra rows, because
        // the caller re-runs the ORIGINAL query, which re-applies
        // every predicate; it must never lose a matching row. An
        // untranslatable predicate is skipped, not fatal — a
        // shorter conjunction is still a superset filter. A
        // positional predicate (index/range) ends the run:
        // predicates after it filter a positionally-selected
        // subsequence.
        let mut conds = Vec::new();
        for p in preds {
            if self.prop_s(p, "kind") != "expr" {
                break;
            }
            match self.predicate_cond(p, None) {
                Ok(c) => conds.push(c),
                Err(_) => {
                    if let Some(c) = self.prefilter_cond(p) {
                        conds.push(c);
                    }
                }
            }
        }
        if conds.is_empty() {
            return Err(SqlError::Unsupported(
                "no leading expression predicates to push".into(),
            ));
        }
        Ok(Partial {
            table,
            where_sql: conds.join(" AND "),
        })
    }

    /// The relaxed prefilter for one refused expression predicate:
    /// the AND of whatever conjuncts translate, each a superset of
    /// the rows its Quarb counterpart keeps. None when nothing
    /// does — the predicate stays entirely on the engine.
    fn prefilter_cond(&mut self, p: NodeId) -> Option<String> {
        let parts: Vec<String> = self
            .arbor
            .children(p)
            .into_iter()
            .filter_map(|c| {
                self.pred_expr(c, None)
                    .ok()
                    .or_else(|| self.prefilter_expr(c))
            })
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" AND "))
        }
    }

    /// One conjunct's superset prefilter — fixed-path JSON shapes
    /// only, per dialect. Every form here is proven to keep every
    /// row the graft would match (extras are fine; the engine
    /// re-checks). The proofs lean on three graft facts: absent
    /// paths match no comparison; `value_eq`/`value_cmp` coerce
    /// numeric-looking strings numerically; and Str-to-Str
    /// ordering is bytewise.
    fn prefilter_expr(&mut self, e: NodeId) -> Option<String> {
        let dialect = self.dialect?;
        match self.kind(e).as_str() {
            // A bare truthy path: existence (with `::`, value
            // truthiness — existence is its superset). The type
            // probe sees JSON null too, as the graft does.
            "path" => {
                let (col, path) = self.json_path_loose(e)?;
                let probe = json_type_probe(dialect, None, &col, &path)?;
                Some(json_valid_guard(
                    dialect,
                    None,
                    &col,
                    format!("{probe} IS NOT NULL"),
                ))
            }
            "compare" => {
                let op = self.prop_s(e, "op");
                let kids = self.arbor.children(e);
                if kids.len() != 2 {
                    return None;
                }
                for (pi, li) in [(0usize, 1usize), (1, 0)] {
                    let Some((col, path)) = self.json_path(kids[pi]) else {
                        continue;
                    };
                    if self.kind(kids[li]) != "literal" {
                        continue;
                    }
                    let ty = self.prop_s(kids[li], "type");
                    if ty == "null" {
                        return None;
                    }
                    let raw = self
                        .prop(kids[li], "value")
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    let op = if pi == 0 { op.clone() } else { flip_op(&op) };
                    return self.json_prefilter(dialect, &col, &path, &op, &ty, &raw);
                }
                None
            }
            _ => None,
        }
    }

    /// Build the dialect's superset prefilter for
    /// `json_path OP literal`.
    fn json_prefilter(
        &self,
        d: Dialect,
        col: &str,
        path: &[String],
        op: &str,
        lit_ty: &str,
        raw: &str,
    ) -> Option<String> {
        if !matches!(op, "=" | "!=" | "<" | "<=" | ">" | ">=") {
            return None;
        }
        // `!=` matches every present path whose value isn't the
        // literal — including JSON null and cross-typed values —
        // so its floor is existence.
        if op == "!=" {
            let probe = json_type_probe(d, None, col, path)?;
            return Some(json_valid_guard(
                d,
                None,
                col,
                format!("{probe} IS NOT NULL"),
            ));
        }
        let numeric_lit = raw.trim().parse::<f64>().is_ok_and(|f| f.is_finite())
            && (lit_ty != "text" || !raw.trim().is_empty());
        if numeric_lit {
            let num: f64 = raw.trim().parse().ok()?;
            // Past 2^53 the engines' exact integers and the
            // graft's f64 part ways at the boundary.
            if num.abs() >= 9_007_199_254_740_992.0 {
                return None;
            }
            let lit = raw.trim();
            match d {
                // Typed extract: JSON numbers compare numerically
                // (both sides parse the same source text to f64);
                // string values escape through the type probe —
                // the graft may coerce them, the server must not
                // drop them.
                Dialect::Sqlite => {
                    let ex = json_extract(d, None, col, path);
                    let ty = json_type_probe(d, None, col, path)?;
                    Some(json_valid_guard(
                        d,
                        None,
                        col,
                        format!("({ex} {op} {lit} OR {ty} = 'text')"),
                    ))
                }
                // float8 is the graft's f64; jsonb numbers print
                // exactly, so the cast parses the same decimal.
                // Strings escape; everything else can't match.
                Dialect::Postgres => {
                    let text = json_extract(d, None, col, path);
                    let ty = json_type_probe(d, None, col, path)?;
                    Some(format!(
                        "(CASE WHEN {ty} = 'number' THEN ({text})::float8 {op} {lit} \
                         WHEN {ty} = 'string' THEN true ELSE false END)"
                    ))
                }
                Dialect::MySql | Dialect::Mssql | Dialect::Oracle => None,
            }
        } else if lit_ty == "text" {
            if raw.contains('\'') || raw.contains('\\') {
                return None;
            }
            match d {
                // Typed extract again: string values order bytewise
                // on both sides; numbers and booleans sit below
                // text in SQLite's cross-type order, so `<` keeps
                // them (extras, re-checked) and `>` drops them
                // (the graft refuses to order Str against them).
                Dialect::Sqlite => {
                    let ex = json_extract(d, None, col, path);
                    Some(json_valid_guard(
                        d,
                        None,
                        col,
                        format!("{ex} {op} '{raw}'"),
                    ))
                }
                // Text ordering elsewhere runs into collation; the
                // engine keeps those.
                _ => None,
            }
        } else {
            None
        }
    }

    /// [`json_path`], loosened for existence: the projection may
    /// be absent (a bare structural path) as well as the bare
    /// `::`.
    fn json_path_loose(&self, o: NodeId) -> Option<(String, Vec<String>)> {
        if self.json_path(o).is_some() {
            return self.json_path(o);
        }
        if self.kind(o) != "path" || self.kid(o, "projection").is_some() {
            return None;
        }
        let steps = self.kids(o, "step");
        if steps.len() < 2 {
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
            let plain = !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !plain {
                return None;
            }
            names.push(name);
        }
        let col = names.remove(0);
        Some((col, names))
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
            // Driver-first: the query's own branch is the FROM
            // table (the driver); the correlation entry is the
            // joined table, whose `$$` equalities form the ON.
            let (ltable, lpreds) = self.table_branch(branches[0])?;
            let (rtable, rpreds) = self.table_branch(self.kids(*corr, "branch")[0])?;
            // The SQL renderings of the two table names (validated
            // or quoted); the raw names stay in the plan metadata,
            // which the driver matches against its catalog.
            let lsql = self.sql_ident(&ltable, "table name")?;
            let rsql = self.sql_ident(&rtable, "table name")?;
            self.from_sql = lsql.clone();
            self.join_sql = Some(rsql.clone());
            // The driver's own predicates are plain WHERE.
            let driver_conds: Vec<NodeId> = lpreds;
            // Split the joined expression's predicates: `$$`
            // equalities form the ON, the rest the WHERE.
            let mut on = Vec::new();
            let mut wheres = Vec::new();
            for p in rpreds {
                self.split_join_pred(p, &rsql, &mut on, &mut wheres)?;
            }
            if on.is_empty() {
                return Err(SqlError::Unsupported(
                    "a correlation without a '$$' equality (no JOIN condition)".into(),
                ));
            }
            for p in driver_conds {
                let cond = self.predicate_cond(p, Some(&lsql.clone()))?;
                wheres.push(cond);
            }
            self.notes.push(
                "JOIN: Quarb's binding is existential (one row per FROM row, \
                 bound to its first witness); SQL multiplies rows when \
                 several joined rows match"
                    .to_string(),
            );
            sel.from = lsql.clone();
            self.from_table = ltable;
            self.join_table = Some(rtable);
            sel.join = Some((rsql.clone(), on.join(" AND ")));
            sel.wheres = wheres;
            // A terminal projection on the driving branch is a
            // one-column select of the FROM table.
            if let Some(proj) = self.kid(branches[0], "projection") {
                let col = self.projection_col(proj)?;
                let col = self.sql_ident(&col, "column name")?;
                sel.select.push(format!("{lsql}.{col}"));
            }
            if self.kid(self.kids(*corr, "branch")[0], "projection").is_some() {
                return Err(SqlError::Unsupported(
                    "a projection on the joined expression (project the witness \
                     in the pipeline: '$*1::col')"
                        .into(),
                ));
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
                    return Err(SqlError::Semantics(
                        "SQL NOT propagates UNKNOWN, dropping the NULL rows \
                         Quarb keeps"
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
                // fixed object path, string equality, and a literal
                // that does NOT look numeric — is provably identical
                // to the client-side graft (each engine's scalar
                // extractor unquotes to text, and an absent path or
                // non-string value excludes the row on both sides,
                // matching Quarb's `value_eq` on non-numeric text).
                // Numeric casts, `!=`, wildcards, and deeper
                // predicates fall through to the ordinary operand
                // logic below, which refuses the navigation; the
                // partial-pushdown prefilter ladder picks those up
                // with superset semantics. Enabled only when a
                // dialect is set.
                if op == "="
                    && let Some(dialect) = self.dialect
                {
                    for (pi, li) in [(0usize, 1usize), (1, 0)] {
                        if self.is_text_literal(kids[li])
                            && let Some((col, path)) = self.json_path(kids[pi])
                        {
                            // Quarb's value_eq compares numeric-looking
                            // strings numerically — '150' matches the
                            // JSON number 150, and the string "150.0".
                            // No engine's text extractor coerces that
                            // way, so the pushed compare would drop
                            // rows the graft keeps. Refuse; the
                            // prefilter ladder handles it as a
                            // superset.
                            let raw = self
                                .prop(kids[li], "value")
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            if raw.trim().parse::<f64>().is_ok() {
                                return Err(SqlError::Semantics(
                                    "a numeric-looking text literal against a \
                                     JSON path compares numerically in Quarb \
                                     (value coercion); no SQL extractor \
                                     matches that"
                                        .into(),
                                ));
                            }
                            let lit = self.operand(kids[li], qual)?;
                            let extract = json_extract(dialect, qual, &col, &path);
                            return Ok(json_valid_guard(
                                dialect,
                                qual,
                                &col,
                                format!("{extract} = {lit}"),
                            ));
                        }
                    }
                }
                // Any other JSON-path comparison: name the real
                // reason, not the generic flat-rows refusal — only
                // fixed-path string equality is provably identical
                // (Quarb coerces numeric-looking values; no
                // engine's extractor matches that). The partial
                // prefilter ladder covers these shapes.
                if self.dialect.is_some() {
                    for side in [kids[0], kids[1]] {
                        if self.json_path(side).is_some()
                            || self.json_path_loose(side).is_some()
                        {
                            return Err(SqlError::Semantics(
                                "a JSON-path comparison pushes whole only as \
                                 fixed-path string equality (Quarb coerces \
                                 numeric-looking values; no extractor matches \
                                 that); this shape rides the partial prefilter"
                                    .into(),
                            ));
                        }
                    }
                }
                let l = self.operand(kids[0], qual)?;
                // Ruling #33: a pattern literal on `=` pushes as
                // LIKE — a `%` per star, each literal segment
                // escaped, in order. Anchoring matches exactly:
                // LIKE anchors both ends, a flanking star lifts
                // one. The case-folding caveat is `*=`'s; `!=`
                // with a pattern rides the scan (NOT LIKE drops
                // the NULL rows Quarb keeps).
                if self.kind(kids[1]) == "pattern" {
                    if op.as_str() != "=" {
                        return Err(SqlError::Unsupported(format!(
                            "a pattern literal on '{op}' (only '=' pushes)"
                        )));
                    }
                    // PostgreSQL's LIKE is case-sensitive under its
                    // deterministic collations — provably what the
                    // pattern test does — so the strict push is
                    // sound there (and a prefix pattern is
                    // index-sargable server-side). Everywhere else
                    // case folding is ambient state (SQLite pragma,
                    // MySQL/MSSQL collation), so strict refuses.
                    if self.strict && self.dialect != Some(Dialect::Postgres) {
                        return Err(SqlError::Semantics(
                            "LIKE case folding differs per engine \
                             (provably case-sensitive only on PostgreSQL)"
                                .into(),
                        ));
                    }
                    if !self.strict {
                        self.notes.push(
                            "pattern → LIKE: Quarb's pattern test is \
                             case-sensitive; LIKE folds case on \
                             SQLite/MySQL but not PostgreSQL"
                                .to_string(),
                        );
                    }
                    let mut like = String::new();
                    for kid in self.arbor.children(kids[1]) {
                        match self.kind(kid).as_str() {
                            "star" => like.push('%'),
                            _ => {
                                let raw = self
                                    .prop(kid, "value")
                                    .map(|v| v.to_string())
                                    .unwrap_or_default();
                                like.push_str(
                                    &raw.replace('\\', "\\\\")
                                        .replace('%', "\\%")
                                        .replace('_', "\\_")
                                        .replace('\'', "''"),
                                );
                            }
                        }
                    }
                    return Ok(format!("{l} LIKE '{like}' ESCAPE '\\'"));
                }
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
                            return Err(SqlError::Semantics(
                                "'!=' drops the NULL rows Quarb keeps \
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
                        // The same PostgreSQL rung as the pattern
                        // literal above: LIKE is provably
                        // case-sensitive there, so `*=` pushes
                        // strict too.
                        if self.strict && self.dialect != Some(Dialect::Postgres) {
                            return Err(SqlError::Semantics(
                                "LIKE case folding differs per engine \
                                 (provably case-sensitive only on PostgreSQL)"
                                    .into(),
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
                        return Err(SqlError::Semantics(
                            "regex matching means a different REGEXP engine per \
                             backend; regexes run engine-side"
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
                    return Err(SqlError::Semantics(
                        "truthiness diverges (0 and '' are falsy in Quarb)".into(),
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
                            return Err(SqlError::Semantics(
                                "a text literal with a quote or backslash \
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
                    if self.prop_s(s, "axis") == "-->" {
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
                // `$*1::col` — the joined expression's witness, in
                // pipeline position: the joined table's column.
                // Anything else is outside the verified-safe set.
                let p = self
                    .kid(o, "projection")
                    .ok_or_else(|| SqlError::Unsupported("a bare '$*' reference".into()))?;
                let col = self.projection_col(p)?;
                let col = self.sql_ident(&col, "column name")?;
                if self.in_on {
                    return Err(SqlError::Unsupported(
                        "a '$*' reference inside the ON (the driver is '$$')".into(),
                    ));
                }
                let Some(join) = self.join_sql.clone() else {
                    return Err(SqlError::Unsupported(
                        "a '$*' reference outside a correlation join".into(),
                    ));
                };
                match self.prop(o, "index") {
                    Some(Value::Int(1)) => Ok(format!("{join}.{col}")),
                    None => Err(SqlError::Unsupported("a bare '$*' reference".into())),
                    Some(v) => Err(SqlError::Unsupported(format!(
                        "pushdown: $*{v} beyond a two-branch correlation"
                    ))),
                }
            }
            "outer" => {
                // `$$::col` — the driver, legal only inside the
                // joined expression's ON bracket (elsewhere the
                // engine reads an enclosing subcontext scope).
                if !self.in_on {
                    return Err(SqlError::Unsupported(
                        "a '$$' reference outside the join's ON".into(),
                    ));
                }
                let kids = self.arbor.children(o);
                let inner = *kids
                    .first()
                    .ok_or_else(|| SqlError::Unsupported("an empty '$$' reference".into()))?;
                if self.kind(inner) != "path" || self.kid(inner, "step").is_some() {
                    return Err(SqlError::Unsupported(
                        "a '$$' reference beyond a plain column ($$::col)".into(),
                    ));
                }
                let p = self
                    .kid(inner, "projection")
                    .ok_or_else(|| SqlError::Unsupported("a bare '$$' reference".into()))?;
                let col = self.projection_col(p)?;
                let col = self.sql_ident(&col, "column name")?;
                Ok(format!("{}.{col}", self.from_sql.clone()))
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
            let uses_driver = self.subtree_has(e, "outer");
            self.in_on = true;
            let cond = self.pred_expr(e, Some(rtable));
            self.in_on = false;
            let cond = cond?;
            if uses_driver && self.kind(e) == "compare" && self.prop_s(e, "op") == "=" {
                // Record which joined-table columns the ON binds —
                // the uniqueness obligation (see
                // Pushdown::join_left). Collected from the arbor's
                // bare-column operand nodes, never from the
                // rendered SQL text, so neither a literal nor an
                // unusual column name can corrupt the obligation.
                self.collect_join_cols(e)?;
                on.push(cond);
            } else {
                wheres.push(cond);
            }
        }
        Ok(())
    }

    /// The joined-table columns the ON equality binds: every bare
    /// column operand (`::col`) in `e`'s subtree, appended to the
    /// join obligation with their raw (catalog) names.
    fn collect_join_cols(&mut self, e: NodeId) -> Result<(), SqlError> {
        // The `$$…` side is the driver — not part of the joined
        // table's obligation.
        if self.kind(e) == "outer" {
            return Ok(());
        }
        if self.kind(e) == "path"
            && self.kid(e, "step").is_none()
            && let Some(p) = self.kid(e, "projection")
        {
            let col = self.projection_col(p)?;
            self.join_on_cols.push(col);
        }
        for c in self.arbor.children(e) {
            self.collect_join_cols(c)?;
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
                    pending_col = Some(self.operand(kids[0], join.map(|(l, _)| l))?);
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
                            // sort/unique/top and kin are spellable
                            // in SQL but with the backend's own
                            // collation and duplicate semantics —
                            // name the divergence, not a missing
                            // feature.
                            if matches!(other, "sort" | "sort_by" | "unique" | "top" | "bottom") {
                                return Err(SqlError::Semantics(format!(
                                    "'{other}' means the backend's collation and \
                                     duplicate semantics; one engine-side ordering \
                                     keeps it identical everywhere"
                                )));
                            }
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
                                return Err(SqlError::Semantics(
                                    "GROUP BY result order is unordered in SQL".into(),
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
                                return Err(SqlError::Semantics(
                                    "ORDER BY collations differ per engine".into(),
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
                            let e = self.operand(kids[0], join.map(|(l, _)| l))?;
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
                                return Err(SqlError::Semantics(
                                    "ORDER BY collations differ per engine".into(),
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
                            let e = self.operand(kids[1], join.map(|(l, _)| l))?;
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
        self.operand(*key, join.map(|(l, _)| l))
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
                let value = self.operand(kids[i + 1], join.map(|(l, _)| l))?;
                let name = self.alias(&name)?;
                fields.push(format!("{value} AS {name}"));
                i += 2;
            } else {
                let value = self.operand(k, join.map(|(l, _)| l))?;
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
    fn pattern_literal_pushes_as_like() {
        // Ruling #33: segments in order, a `%` per star, LIKE's
        // metacharacters escaped. Strict push only where LIKE is
        // provably case-sensitive (PostgreSQL); everywhere else the
        // pattern rides the scan.
        let q = "/users/*[::name = 'app-'*]::id";
        let p = pushdown(q, Some(Dialect::Postgres)).unwrap();
        assert_eq!(
            p.sql,
            "SELECT id FROM users WHERE name LIKE 'app-%' ESCAPE '\\'"
        );
        assert!(pushdown(q, Some(Dialect::Sqlite)).is_none());
        assert!(pushdown(q, Some(Dialect::MySql)).is_none());
        // contains and multi-segment shapes
        let p = pushdown("/users/*[::name = *'web'*]::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%web%'"), "{}", p.sql);
        let p = pushdown("/users/*[::name = *'a'*'.gz']::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%a%.gz'"), "{}", p.sql);
        // LIKE metacharacters in a segment are escaped
        let p = pushdown("/users/*[::name = *'50%_x'*]::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%50\\%\\_x%'"), "{}", p.sql);
        // `!=` with a pattern rides the scan
        assert!(
            pushdown("/users/*[::name != *'web'*]::id", Some(Dialect::Postgres)).is_none()
        );
        // the `*=` alias gains the same PostgreSQL rung
        let p = pushdown("/users/*[::name *= 'web']::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%web%'"), "{}", p.sql);
        assert!(pushdown("/users/*[::name *= 'web']::id", Some(Dialect::Sqlite)).is_none());
    }

    #[test]
    fn json_column_path_pushdown_per_dialect() {
        // `[/col/a/b:: = 'lit']` navigates into a JSON column; each
        // dialect extracts the fixed path to text and compares.
        // Verified live against all five engines to match the graft.
        let q = "/orders/*[/data/meta/tier:: = 'gold']::id";
        let sql = |d| pushdown(q, Some(d)).unwrap().sql;
        assert_eq!(
            sql(Dialect::Postgres),
            "SELECT id FROM orders WHERE ((data::jsonb #>> '{meta,tier}') = 'gold')"
        );
        assert_eq!(
            sql(Dialect::MySql),
            "SELECT id FROM orders WHERE (JSON_VALID(data) AND \
             JSON_UNQUOTE(JSON_EXTRACT(data, '$.meta.tier')) = 'gold')"
        );
        assert_eq!(
            sql(Dialect::Sqlite),
            "SELECT id FROM orders WHERE (json_valid(data) AND \
             json_extract(data, '$.meta.tier') = 'gold')"
        );
        assert_eq!(
            sql(Dialect::Mssql),
            "SELECT id FROM orders WHERE (ISJSON(data) = 1 AND \
             JSON_VALUE(data, '$.meta.tier') = 'gold')"
        );
        assert_eq!(
            sql(Dialect::Oracle),
            "SELECT id FROM orders WHERE (JSON_VALUE(data, '$.meta.tier') = 'gold')"
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
    fn json_pushdown_refuses_numeric_looking_literal() {
        // value_eq coerces numeric-looking strings ('150' matches
        // the JSON number 150 and the string "150.0"); no engine's
        // extractor does, so the push would drop rows.
        let q = "/orders/*[/data/total:: = '150']::id";
        assert!(pushdown(q, Some(Dialect::Sqlite)).is_none());
        assert!(pushdown(q, Some(Dialect::Postgres)).is_none());
    }

    #[test]
    fn partial_json_prefilters_sqlite() {
        // Exact string equality rides the strict gate.
        let p = partial_pushdown(
            "/orders/*[/payload/customer/geo/city:: = 'Lyon']::id",
            Some(Dialect::Sqlite),
        )
        .unwrap();
        assert_eq!(p.table, "orders");
        assert_eq!(
            p.where_sql,
            "(json_valid(payload) AND \
             json_extract(payload, '$.customer.geo.city') = 'Lyon')"
        );
        // Numeric comparison: typed compare, text values escape.
        let p = partial_pushdown(
            "/orders/*[/payload/total:: > 150]::id",
            Some(Dialect::Sqlite),
        )
        .unwrap();
        assert_eq!(
            p.where_sql,
            "(json_valid(payload) AND (json_extract(payload, '$.total') > 150 \
             OR json_type(payload, '$.total') = 'text'))"
        );
        // Bare existence.
        let p = partial_pushdown("/orders/*[/payload/gift]::id", Some(Dialect::Sqlite)).unwrap();
        assert_eq!(
            p.where_sql,
            "(json_valid(payload) AND json_type(payload, '$.gift') IS NOT NULL)"
        );
        // != floors at existence.
        let p = partial_pushdown(
            "/orders/*[/payload/status:: != 'x']::id",
            Some(Dialect::Sqlite),
        )
        .unwrap();
        assert_eq!(
            p.where_sql,
            "(json_valid(payload) AND json_type(payload, '$.status') IS NOT NULL)"
        );
        // A flipped literal flips the operator.
        let p = partial_pushdown(
            "/orders/*[150 < /payload/total::]::id",
            Some(Dialect::Sqlite),
        )
        .unwrap();
        assert!(p.where_sql.contains("> 150"), "{}", p.where_sql);
        // Without a dialect, JSON predicates stay on the engine.
        assert!(partial_pushdown("/orders/*[/payload/gift]::id", None).is_none());
    }

    #[test]
    fn partial_json_prefilters_postgres() {
        let p = partial_pushdown(
            "/orders/*[/payload/total:: > 150]::id",
            Some(Dialect::Postgres),
        )
        .unwrap();
        assert_eq!(
            p.where_sql,
            "(CASE WHEN jsonb_typeof(payload::jsonb #> '{total}') = 'number' \
             THEN ((payload::jsonb #>> '{total}'))::float8 > 150 \
             WHEN jsonb_typeof(payload::jsonb #> '{total}') = 'string' \
             THEN true ELSE false END)"
        );
        let p =
            partial_pushdown("/orders/*[/payload/gift]::id", Some(Dialect::Postgres)).unwrap();
        assert_eq!(
            p.where_sql,
            "(jsonb_typeof(payload::jsonb #> '{gift}') IS NOT NULL)"
        );
    }

    #[test]
    fn partial_skips_untranslatable_keeps_prefix() {
        // A refused predicate no prefilter covers is skipped — the
        // shorter conjunction is still a superset (the engine
        // re-applies the original) — rather than aborting the plan.
        let p = partial_pushdown(
            "/orders/*[::status = 'shipped'][/payload/line_items/*/sku:: = 'X']::id",
            Some(Dialect::Sqlite),
        )
        .unwrap();
        assert_eq!(p.where_sql, "status = 'shipped'");
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
        assert!(partial_pushdown(&format!("/t/*[::x != 5]{GROUPED}"), None).is_none());
        assert!(partial_pushdown(&format!("/t/*[!::x = 5]{GROUPED}"), None).is_none());
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
        assert!(partial_pushdown(&format!("/t/*[::name = \"it's\"]{GROUPED}"), None).is_none());
        assert_eq!(
            pushdown("/t/*[::name = \"rare\"] | ::name", None).unwrap().sql,
            "SELECT name FROM t WHERE name = 'rare'"
        );
    }
}
