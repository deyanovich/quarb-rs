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
//! Deliberately refused: `-->` resolution chains — the foreign-key
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

use crate::{Refusal, SqlError, Translation};
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
        join_every: false,
        column_types: ColumnTypes::default(),
        from_sql: String::new(),
        join_sql: None,
        in_on: false,
        order: PlanOrder::default(),
        aggregate: false,
        keyed_hops: false,
        from_marks: Vec::new(),
        join_marks: Vec::new(),
        join_alias: None,
        sums: Vec::new(),
    };
    let query = ex.query()?;
    Ok(Translation {
        query,
        notes: ex.notes,
    })
}

/// A part SQL has no form for, under the word its refusal is told
/// by.
fn not_pushed(word: Refusal, reason: String) -> SqlError {
    SqlError::NotPushed {
        word,
        mismatch: false,
        reason,
    }
}

/// A part SQL can spell, with other semantics: the reason names the
/// divergence.
fn mismatch(word: Refusal, reason: String) -> SqlError {
    SqlError::NotPushed {
        word,
        mismatch: true,
        reason,
    }
}

/// The exporter rewrites `__LEFT__` (the join's left table) and
/// `__AGG__` (the HAVING aggregate) as internal placeholders.
/// Query text containing either marker would be rewritten inside
/// its own string literals — and could spoof the emitted SQL — so
/// such queries stay on the scan path.
fn refuse_marker(quarb: &str) -> Result<(), SqlError> {
    for marker in ["__LEFT__", "__AGG__"] {
        if quarb.contains(marker) {
            return Err(not_pushed(
                Refusal::Name,
                format!("query text contains the reserved marker \"{marker}\""),
            ));
        }
    }
    Ok(())
}

/// A pushdown plan: SQL whose execution is provably identical to
/// the Quarb query's — plus the table whose primary key must order
/// the rows (`None` for a single aggregate row, where order is
/// moot). The driver appends the `ORDER BY`, since the key lives in
/// its catalog.
/// A sum or a mean over a column, as a plan's statement writes it
/// (`text`: `SUM(unit_price)`). The engine adds a float column by
/// the careful sum, in key order; SQLite adds the same way, and its
/// SUM is the engine's to the last digit. PostgreSQL and DuckDB add
/// plainly and across workers, in no fixed order, so a float sum
/// there moves in its last digits from one run to the next: their
/// doors ask the catalog whether the column is a float and rewrite
/// the sum in the dialect's ordered form (`sum(x ORDER BY key)`),
/// which is the same number every run. The plan says which sums it
/// holds; the door does the rest.
///
/// `table` is the table the column is looked up in. `order` is what
/// the scan adds by: the tables whose keys order the rows, each with
/// the name the statement gives it (the driver; after it the joined
/// table where every witness is bound, under its alias where it is
/// joined to itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatSum {
    pub text: String,
    pub mean: bool,
    pub table: String,
    pub column: String,
    pub column_sql: String,
    pub order: Vec<(String, String)>,
}

pub struct Pushdown {
    pub sql: String,
    /// The plan's sums and means over a column, for a source that
    /// adds floats in its own order: see [`FloatSum`].
    pub sums: Vec<FloatSum>,
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
    /// Present when the plan is an every-witness JOIN (`<=>*`,
    /// ruling 2026-11-11): the joined table. Such a JOIN is what the
    /// query asks, so it carries no uniqueness obligation
    /// (`join_left` is absent); its obligation is the ORDER: the
    /// rows of one driver stand in the joined table's own order,
    /// after the driver's (`order_table`'s key, then this table's).
    /// A driver that cannot order by both must decline the plan.
    pub order_then: Option<String>,
    /// The name that second table bears in the statement where it
    /// is not its own: a table joined to itself is aliased, and the
    /// driver writes its key under the alias.
    pub order_then_as: Option<String>,
    /// The plan's own sort, in SQL, where it has one: `(n IS NULL),
    /// n` for `sort_by(::n)`, a null key last as the engine puts it.
    /// The driver writes it before the keys, which keep the ties in
    /// the tables' order as the engine's stable sort does.
    pub order_first: Option<String>,
    /// The keys descending: the context reversed (`@| reverse`).
    pub order_desc: bool,
    /// The plan is a fold: one row a group, in the order each group
    /// first appears, which the driver writes as the least key of
    /// each (`ORDER BY MIN(key)`).
    pub order_groups: bool,
    /// The first rows alone (`@| [..n]`), after the order.
    pub limit: Option<u64>,
    /// The plan's rows are records the query built (`| %(…)`, the
    /// `%.` of a fold): a row of one column is then a record of one
    /// field, where a projection's is the bare value.
    pub record: bool,
    /// The plan's rows are the records of the rows' own properties
    /// (`| %`): a NULL cell is no property of its row, and is left
    /// out of that row's record, as the scan leaves it out.
    pub properties: bool,
    /// The column a sort, a fold or a distinct count goes by, with
    /// the word of that construct. The plan is the scan's only where
    /// the catalog says the column is numeric (it is among
    /// `column_types.numeric`), and unlike a comparison the database
    /// would not refuse it over a text column: it would run, and
    /// answer its collation's order or its own groups. A caller with
    /// no catalog to ask scans; one whose catalog says the column is
    /// not numeric scans and tells of it under the word.
    pub keyed_by: Option<(Refusal, String)>,
    /// What the plan takes for granted about column types. Quarb
    /// compares by value (a numeric-looking text and a number meet
    /// as numbers, a number and other text never meet); SQL
    /// compares by the column's type, and the loosely typed engines
    /// (SQLite, MySQL) coerce by rules of their own. The plan is
    /// the scan's only where the columns it compares with a number
    /// are numeric-typed and those it compares with text are not:
    /// the caller asks its catalog and scans when one is otherwise.
    /// (A strictly typed engine rejects the mismatch itself, which
    /// sends the caller to the scan as well.)
    pub column_types: ColumnTypes,
    /// The tables the plan reads (the FROM table, then a joined one).
    pub tables: Vec<String>,
}

/// The column types a pushed plan assumes (see
/// [`Pushdown::column_types`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnTypes {
    /// Columns compared with a numeric literal.
    pub numeric: Vec<String>,
    /// Columns compared with a text literal.
    pub text: Vec<String>,
    /// Columns the plan extracts a JSON path from on the strength
    /// of the column's declaration alone (the step carried no
    /// `<json>`): the pushed `json_extract` is the scan's only where
    /// the catalog declares the column json, since the scan grafts
    /// a leaf only as declared (spec: Grafting).
    pub json: Vec<String>,
    /// Whether the plan reads a JSON value as a child node of its
    /// column (`/col/key::`, or the bare `/col/key` of an existence
    /// test), with `<json>` on the step or without: the scan's only
    /// where the mount's grafts list scalars as children, which the
    /// property reading (`?graft=json2`) does not. The property
    /// form (`/col::key`) answers under both readings and does not
    /// set it. No assumption about the catalog ([`is_empty`] and
    /// [`unmet`] leave it out): the front end holds it against its
    /// mount's reading.
    ///
    /// [`is_empty`]: Self::is_empty
    /// [`unmet`]: Self::unmet
    pub json_children: bool,
    /// Whether the plan compares two values of the row with each
    /// other (`[::a = ::b]`, a join's key equality, a column against
    /// an arithmetic): the engine compares them by value, SQL by
    /// their types, and the two agree only where every column
    /// compared is numeric. The columns are among [`numeric`]; this
    /// says that no typed engine enforces the assumption by itself
    /// (two text columns compare without complaint, as texts), so a
    /// source whose catalog is not asked does not get the plan.
    ///
    /// [`numeric`]: Self::numeric
    pub columns_compared: bool,
    /// The columns among [`numeric`] that a computation assumed
    /// (`::a + 1`), and those a reducer assumed (`::a @| sum`): what
    /// a front end names when it tells an assumption the catalog did
    /// not bear out. A column in neither was assumed by a
    /// comparison.
    ///
    /// [`numeric`]: Self::numeric
    pub computed: Vec<String>,
    pub reduced: Vec<String>,
}

/// One column of a source's catalog, as a plan's assumptions are
/// held against it: its name; whether its type is numeric; whether
/// it is declared json; and whether the database compares its text
/// exactly, by characters, case and trailing spaces included (false
/// for a `char(n)` of PostgreSQL, which ignores the padding it
/// answers with, and for a column of SQLite declared under a
/// collation).
pub type ColumnFact = (String, bool, bool, bool);

impl ColumnTypes {
    pub fn is_empty(&self) -> bool {
        self.numeric.is_empty() && self.text.is_empty() && self.json.is_empty()
    }

    /// The assumptions a catalog does not bear out: the columns
    /// compared with a number (or with each other, or computed
    /// with) that are not numeric-typed, and those compared with
    /// text that are, or whose text the database does not compare
    /// exactly. `catalog` lists every column of the tables read
    /// ([`ColumnFact`]); names meet without regard to case. A column
    /// the catalog does not list is unknown, and counts as unmet.
    pub fn unmet(&self, catalog: &[ColumnFact]) -> Vec<String> {
        let met = |column: &str, pick: fn(&ColumnFact) -> bool, want: bool| {
            let mut found = catalog
                .iter()
                .filter(|(name, ..)| name.eq_ignore_ascii_case(column))
                .peekable();
            found.peek().is_some() && found.all(|entry| pick(entry) == want)
        };
        let numeric = |e: &ColumnFact| e.1;
        let json = |e: &ColumnFact| e.2;
        // a text comparison: no number, and compared exactly
        let exact_text = |e: &ColumnFact| !e.1 && e.3;
        let mut out: Vec<String> = Vec::new();
        for (columns, pick, want) in [
            (&self.numeric, numeric as fn(&ColumnFact) -> bool, true),
            (&self.text, exact_text, true),
            (&self.json, json, true),
        ] {
            for c in columns {
                if !met(c, pick, want) && !out.contains(c) {
                    out.push(c.clone());
                }
            }
        }
        out
    }
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
        join_every: false,
        column_types: ColumnTypes::default(),
        from_sql: String::new(),
        join_sql: None,
        in_on: false,
        order: PlanOrder::default(),
        aggregate: false,
        keyed_hops: false,
        from_marks: Vec::new(),
        join_marks: Vec::new(),
        join_alias: None,
        sums: Vec::new(),
    };
    let sql = ex.query()?;
    let order_table = if ex.aggregate && !ex.order.groups {
        None
    } else {
        // Rows come back in the driver's document order — the FROM
        // table's (driver-first correlation).
        Some(ex.from_table.clone())
    };
    // an every-witness JOIN multiplies rows by design: no uniqueness
    // to verify, the joined table's order to keep instead
    let join_left = if ex.join_every {
        None
    } else {
        ex.join_table.clone().map(|t| (t, ex.join_on_cols.clone()))
    };
    let order_then = if ex.join_every && !ex.aggregate {
        ex.join_table.clone()
    } else {
        None
    };
    let mut tables = vec![ex.from_table.clone()];
    tables.extend(ex.join_table.clone());
    // the plan's own sort: a null key last, the ties left to the
    // keys; reversed, every term turns
    let order_first = ex.order.sort.as_ref().map(|(e, desc)| {
        let nulls = if ex.order.reversed { " DESC" } else { "" };
        let dir = if *desc != ex.order.reversed {
            " DESC"
        } else {
            ""
        };
        format!("({e} IS NULL){nulls}, {e}{dir}")
    });
    // A sum's column is the driver's or, under the joined table's
    // name in the statement, the joined table's; the order the scan
    // adds in is the driver's key and, where every witness is bound,
    // the joined table's after it (a first witness is one row a
    // driver).
    let sums: Vec<FloatSum> = {
        let from = ex.from_table.clone();
        let joined: Option<(String, String)> = ex
            .join_table
            .clone()
            .map(|t| (t.clone(), ex.join_alias.clone().unwrap_or(t)));
        let mut order = vec![(from.clone(), from.clone())];
        if let (true, Some(j)) = (ex.join_every, joined.as_ref()) {
            order.push(j.clone());
        }
        ex.sums
            .iter()
            .map(|(mean, column, column_sql)| {
                let table = match (column_sql.rsplit_once('.'), joined.as_ref()) {
                    (Some((qual, _)), Some((t, name))) if qual == name => t.clone(),
                    _ => from.clone(),
                };
                FloatSum {
                    text: format!("{}({column_sql})", if *mean { "AVG" } else { "SUM" }),
                    mean: *mean,
                    table,
                    column: column.clone(),
                    column_sql: column_sql.clone(),
                    order: order.clone(),
                }
            })
            .collect()
    };
    Ok(Pushdown {
        sql,
        sums,
        order_table,
        join_left,
        order_then_as: order_then.as_ref().and(ex.join_alias.clone()),
        order_then,
        order_first,
        order_desc: ex.order.reversed,
        order_groups: ex.order.groups,
        limit: ex.order.limit,
        record: ex.order.record,
        properties: ex.order.properties,
        keyed_by: ex.order.keyed_by.clone(),
        column_types: ex.column_types.clone(),
        tables,
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

/// The columns SQLite's `json_each` answers: inside a subquery over
/// it an unqualified name of these is its own.
const JSON_EACH_COLUMNS: &[&str] = &[
    "key", "value", "type", "atom", "id", "parent", "fullkey", "path", "json", "root",
];

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

/// The literals no match of `pattern` can do without: each is a
/// substring of every text the regex matches. Read from the regex
/// crate's own parse (the engine compiles with its defaults): a
/// literal of a concatenation, of a group, of a repetition that
/// runs at least once. An alternation, a class (a letter under
/// `(?i)` is one) and an optional part promise nothing.
pub(crate) fn required_literals(pattern: &str) -> Vec<String> {
    use regex_syntax::hir::{Hir, HirKind};
    fn walk(h: &Hir, out: &mut Vec<String>) {
        match h.kind() {
            HirKind::Literal(lit) => {
                if let Ok(s) = std::str::from_utf8(&lit.0)
                    && !s.is_empty()
                    && !out.iter().any(|o| o == s)
                {
                    out.push(s.to_string());
                }
            }
            HirKind::Capture(c) => walk(&c.sub, out),
            HirKind::Concat(parts) => parts.iter().for_each(|p| walk(p, out)),
            HirKind::Repetition(r) if r.min >= 1 => walk(&r.sub, out),
            _ => {}
        }
    }
    let Ok(hir) = regex_syntax::parse(pattern) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&hir, &mut out);
    out
}

/// "`l` holds `lit`", as a filter that keeps every row whose text
/// holds the literal by bytes and case, and may keep more (the
/// engine runs the regex over what is fetched):
/// - SQLite's `instr` is exactly that test;
/// - PostgreSQL's `LIKE` is case-sensitive there;
/// - MySQL's `instr` follows the column's collation, which folds
///   case and accents by default: it keeps every text that holds
///   the literal exactly, and some that hold it in another case;
/// - under no dialect (the doors of DuckDB, BigQuery and Athena)
///   `strpos`, which the three have, by characters and case.
///
/// `None` for SQL Server and Oracle, and for a literal a SQL string
/// would not carry as written on every server (a quote, a
/// backslash, a control character).
fn contains_sql(dialect: Option<Dialect>, l: &str, lit: &str) -> Option<String> {
    if lit
        .chars()
        .any(|c| c.is_control() || c == '\\' || c == '\'')
    {
        return None;
    }
    match dialect {
        Some(Dialect::Sqlite | Dialect::MySql) => Some(format!("instr({l}, '{lit}') > 0")),
        Some(Dialect::Postgres) => {
            let pat = lit.replace('%', "\\%").replace('_', "\\_");
            Some(format!("{l} LIKE '%{pat}%' ESCAPE '\\'"))
        }
        None => Some(format!("strpos({l}, '{lit}') > 0")),
        Some(Dialect::Mssql | Dialect::Oracle) => None,
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
    /// The join keeps every FROM row (`<=>?`, the outer
    /// correlation): a LEFT JOIN.
    join_outer: bool,
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
            let kind = if self.join_outer { "LEFT JOIN" } else { "JOIN" };
            out.push_str(&format!(" {kind} {t} ON {on}"));
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
    /// As [`Pushdown::column_types`]: the prefilter keeps every row
    /// the scan would only where these assumptions hold.
    pub column_types: ColumnTypes,
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
/// and no `:` / `:::` metadata anywhere (a filtered `:::n-rows`
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
    partial_pushdown_with(quarb, dialect, false)
}

/// [`partial_pushdown_explained`] for an adapter that reads steps by
/// key and filters only the table's listing
/// (`RelationalModel::with_filtered_listing`): crosslinks and
/// resolutions no longer refuse the prefilter, since a step into the
/// filtered table reads its row unfiltered. What still refuses: a
/// second navigation into the table, and a `<--` or `=>` scope that
/// could list it.
pub fn partial_pushdown_keyed(quarb: &str, dialect: Option<Dialect>) -> Result<Partial, SqlError> {
    partial_pushdown_with(quarb, dialect, true)
}

fn partial_pushdown_with(
    quarb: &str,
    dialect: Option<Dialect>,
    keyed_hops: bool,
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
        join_every: false,
        column_types: ColumnTypes::default(),
        from_sql: String::new(),
        join_sql: None,
        in_on: false,
        order: PlanOrder::default(),
        aggregate: false,
        keyed_hops,
        from_marks: Vec::new(),
        join_marks: Vec::new(),
        join_alias: None,
        sums: Vec::new(),
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
            return Err(not_pushed(
                Refusal::Walk,
                "path patterns (groups and quantifiers)".into(),
            ));
        }
        stack.extend(arbor.children(n));
    }
    Ok(())
}

/// The order a strict plan asks its driver for ([`Pushdown`]'s
/// `order_first`, `order_desc`, `order_groups`, `limit`).
#[derive(Default)]
struct PlanOrder {
    /// `sort_by(::n)`: the key as SQL, and whether `--desc`.
    sort: Option<(String, bool)>,
    /// `@| reverse`.
    reversed: bool,
    /// `@| [..n]`.
    limit: Option<u64>,
    /// A fold by one key.
    groups: bool,
    /// The numeric column the plan sorts, folds or counts distinct
    /// by, and the word of the construct.
    keyed_by: Option<(Refusal, String)>,
    /// The rows are records the query built.
    record: bool,
    /// The records are the rows' own properties (`| %`): a NULL
    /// cell is left out of its row's record.
    properties: bool,
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
    /// The correlation is `<=>*`: every witness, the SQL JOIN itself.
    join_every: bool,
    /// What the comparisons assume of column types (see
    /// [`Pushdown::column_types`]).
    column_types: ColumnTypes,
    from_sql: String,
    join_sql: Option<String>,
    in_on: bool,
    /// The order of a strict plan, for its driver.
    order: PlanOrder,
    aggregate: bool,
    /// The adapter reads steps by key (the pushed-down WHERE filters
    /// the table's listing only), so crosslinks and resolutions are
    /// safe beside the prefilter.
    keyed_hops: bool,
    /// The names the driver's rows are marked under (`/tracks/* .t`)
    /// and the names the joined expression files its witness under
    /// (`<=> /albums/*[…] .al`): `$$.t::col` is the FROM table's
    /// column, `$$.al::col` the joined table's.
    from_marks: Vec<String>,
    join_marks: Vec<String>,
    /// The joined table's name in the statement where it is not its
    /// own: the alias of a table joined to itself.
    join_alias: Option<String>,
    /// The sums and means over a column the plan writes: the mean
    /// flag, the column's name, the column as the statement writes
    /// it. See [`FloatSum`].
    sums: Vec<(bool, String, String)>,
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
            .ok_or_else(|| not_pushed(Refusal::Name, "empty query".into()))?;
        // A correlation does not forbid the prefilter: the driver's
        // own predicates restrict the driver's rows whatever the join
        // then binds. What must not happen is the join (or anything
        // else) reaching the filtered table a second time — a
        // self-join would see only the filtered rows — and the
        // whole-query gates below refuse exactly that: they walk the
        // correlation entries too.
        let branches = self.kids(q, "branch");
        if branches.len() != 1 {
            return Err(not_pushed(Refusal::Union, "a branch union".into()));
        }
        let (table, preds) = self.table_branch_lead(branches[0], self.keyed_hops)?;

        // Whole-query gates.
        let all = self.walk_all(root);
        let mut table_mentions = 0;
        for n in &all {
            match self.kind(*n).as_str() {
                "step" => {
                    let axis = self.prop_s(*n, "axis");
                    // A crosslink, a resolution or a value match may
                    // land in (or search) the filtered table and would
                    // see only its filtered rows: `=>` and the scoped
                    // `<--` carry their scope as text, so the table
                    // count below cannot see them.
                    if matches!(axis.as_str(), "->" | "<-" | "--" | "-->" | "<--" | "=>") {
                        // Where steps are read by key they are exact
                        // beside the prefilter. A scope is a listing,
                        // though: it is safe only when it plainly
                        // lists another table (`/other/…`), and an
                        // unscoped `=>` walks every table.
                        let scope = self.prop_s(*n, "scope");
                        let lists_other_table = scope
                            .strip_prefix('/')
                            .and_then(|rest| rest.split(['/', '[', '<']).next())
                            .is_some_and(|first| {
                                !first.is_empty()
                                    && first != table
                                    && first.chars().all(|c| c.is_alphanumeric() || c == '_')
                            });
                        let safe = self.keyed_hops
                            && if scope.is_empty() {
                                axis != "=>"
                            } else {
                                lists_other_table
                            };
                        if !safe {
                            return Err(not_pushed(
                                Refusal::Condition,
                                "crosslink/resolution axes could reach \
                                 the filtered table"
                                    .into(),
                            ));
                        }
                    }
                    if self.prop_s(*n, "matcher") == table {
                        table_mentions += 1;
                    }
                }
                "projection" if self.prop_s(*n, "kind") != "property" => {
                    return Err(not_pushed(
                        Refusal::Condition,
                        "metadata would observe the filtering \
                         (:::n-rows, :index)"
                            .into(),
                    ));
                }
                _ => {}
            }
        }
        if table_mentions != 1 {
            return Err(not_pushed(
                Refusal::Condition,
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
            return Err(not_pushed(
                Refusal::Nothing,
                "no leading expression predicates to push".into(),
            ));
        }
        Ok(Partial {
            table,
            where_sql: conds.join(" AND "),
            column_types: self.column_types.clone(),
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
        // (the JSON forms are a dialect's; a conjunction, a
        // disjunction and a regex's literals need none)
        let dialect = self.dialect;
        match self.kind(e).as_str() {
            // A bare truthy path: existence (with `::`, value
            // truthiness — existence is its superset). The type
            // probe sees JSON null too, as the graft does.
            "path" => {
                // The property form (`/col/a::b`) is truthy on a
                // scalar and on a list with an item: the key is
                // present either way, so existence is its superset
                // too.
                let dialect = dialect?;
                let (col, path) = self
                    .json_path_loose(e)
                    .or_else(|| self.json_property_path(e))?;
                self.note_json_column(e, &col);
                let probe = json_type_probe(dialect, None, &col, &path)?;
                Some(json_valid_guard(
                    dialect,
                    None,
                    &col,
                    format!("{probe} IS NOT NULL"),
                ))
            }
            // A conjunction keeps what either side keeps and more
            // when one side is left out; a disjunction needs both.
            "and" | "or" => {
                let kind = self.kind(e);
                let sides: Vec<Option<String>> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| {
                        self.pred_expr(c, None)
                            .ok()
                            .or_else(|| self.prefilter_expr(c))
                    })
                    .collect();
                if kind == "or" {
                    let sides: Option<Vec<String>> = sides.into_iter().collect();
                    return sides
                        .filter(|s| !s.is_empty())
                        .map(|s| format!("({})", s.join(" OR ")));
                }
                let sides: Vec<String> = sides.into_iter().flatten().collect();
                (!sides.is_empty()).then(|| format!("({})", sides.join(" AND ")))
            }
            "compare" => {
                let op = self.prop_s(e, "op");
                let kids = self.arbor.children(e);
                if kids.len() != 2 {
                    return None;
                }
                if op == "==" {
                    return self.regex_prefilter(dialect, &kids);
                }
                let dialect = dialect?;
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
                    self.note_json_column(kids[pi], &col);
                    return self.json_prefilter(dialect, &col, &path, &op, &ty, &raw);
                }
                // The property form, `/col/a::b OP literal`.
                for (pi, li) in [(0usize, 1usize), (1, 0)] {
                    let Some((col, path)) = self.json_property_path(kids[pi]) else {
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
                    self.note_json_column(kids[pi], &col);
                    return self.json_property_prefilter(dialect, &col, &path, &op, &ty, &raw);
                }
                // SQL Server: a text column equal to a text literal.
                // The whole plan leaves the comparison to the engine
                // there (the collation folds case, accents and
                // trailing spaces), but what the collation calls
                // equal holds every row the engine calls equal: a
                // superset, which the engine checks again. A literal
                // that reads as a number is left out: the engine
                // meets it with '1.0' and '01' too.
                if dialect == Dialect::Mssql && op == "=" {
                    for (ci, li) in [(0usize, 1usize), (1, 0)] {
                        let plain = self.column_operand(kids[ci]).is_some()
                            && self.is_text_literal(kids[li])
                            && self.prop(kids[li], "value").is_some_and(|v| {
                                Value::Str(v.to_string()).numeric_reading().is_none()
                            });
                        if plain {
                            let l = self.operand(kids[ci], None).ok()?;
                            let r = self.operand(kids[li], None).ok()?;
                            return Some(format!("{l} = {r}"));
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// The superset prefilter for `::col == (/regex/)`: every literal
    /// the regex cannot match without is a substring of the text it
    /// matches, so the rows that hold them all are a superset of the
    /// rows the engine keeps; the engine runs the regex itself over
    /// what is fetched. A NULL holds no substring and matches no
    /// regex. The column is recorded as a text one, for the caller's
    /// catalog check: a number would be searched in the database's
    /// rendering of it, not the engine's.
    fn regex_prefilter(&mut self, dialect: Option<Dialect>, kids: &[NodeId]) -> Option<String> {
        if !self.is_text_literal(kids[1]) {
            return None;
        }
        let column = self.column_operand(kids[0])?;
        let pattern = self.prop(kids[1], "value")?.to_string();
        let literals = required_literals(&pattern);
        let l = self.operand(kids[0], None).ok()?;
        let conds: Vec<String> = literals
            .iter()
            .filter_map(|lit| contains_sql(dialect, &l, lit))
            .collect();
        if conds.is_empty() {
            return None;
        }
        if !self.column_types.text.contains(&column) {
            self.column_types.text.push(column);
        }
        Some(conds.join(" AND "))
    }

    /// The superset prefilter for `/col/a::b OP literal`, the
    /// property form of a JSON path. It is [`json_prefilter`] with
    /// the two places where a property differs from a child's value:
    ///
    /// - an ARRAY of scalars is a list, and a list in a comparison
    ///   matches when any item does (`[::tags = 'a']` keeps
    ///   `{"tags": ["a", "b"]}`); the child form's `/tags::` has no
    ///   value there. So every form lets the arrays through for the
    ///   engine to decide;
    /// - `!=` keeps a row whose key is ABSENT (null is not the
    ///   literal), where the child form has no node and no thread.
    ///   Its floor is the existence of the node the property is
    ///   read on; on the column itself there is none (a cell that
    ///   is no JSON still answers a null property).
    ///
    /// [`json_prefilter`]: Self::json_prefilter
    fn json_property_prefilter(
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
        if op == "!=" {
            let owner = &path[..path.len() - 1];
            if owner.is_empty() {
                return None;
            }
            let probe = json_type_probe(d, None, col, owner)?;
            return Some(json_valid_guard(
                d,
                None,
                col,
                format!("{probe} IS NOT NULL"),
            ));
        }
        let ty = json_type_probe(d, None, col, path)?;
        let numeric_lit = raw.trim().parse::<f64>().is_ok_and(|f| f.is_finite())
            && (lit_ty != "text" || !raw.trim().is_empty());
        if numeric_lit {
            let num: f64 = raw.trim().parse().ok()?;
            if num.abs() >= 9_007_199_254_740_992.0 {
                return None;
            }
            let lit = raw.trim();
            match d {
                Dialect::Sqlite => {
                    let ex = json_extract(d, None, col, path);
                    Some(json_valid_guard(
                        d,
                        None,
                        col,
                        format!("({ex} {op} {lit} OR {ty} IN ('text', 'array'))"),
                    ))
                }
                Dialect::Postgres => {
                    let text = json_extract(d, None, col, path);
                    Some(format!(
                        "(CASE WHEN {ty} = 'number' THEN ({text})::float8 {op} {lit} \
                         WHEN {ty} IN ('string', 'array') THEN true ELSE false END)"
                    ))
                }
                Dialect::MySql | Dialect::Mssql | Dialect::Oracle => None,
            }
        } else if lit_ty == "text" {
            if raw.contains('\'') || raw.contains('\\') {
                return None;
            }
            match d {
                Dialect::Sqlite => {
                    let ex = json_extract(d, None, col, path);
                    Some(json_valid_guard(
                        d,
                        None,
                        col,
                        format!("({ex} {op} '{raw}' OR {ty} = 'array')"),
                    ))
                }
                // Equality alone: a string equals the literal or it
                // does not (a looser collation keeps more, never
                // fewer); ordering runs into collation, as in the
                // child form.
                Dialect::Postgres if op == "=" => {
                    let text = json_extract(d, None, col, path);
                    Some(format!(
                        "(CASE WHEN {ty} = 'string' THEN {text} = '{raw}' \
                         WHEN {ty} = 'array' THEN true ELSE false END)"
                    ))
                }
                _ => None,
            }
        } else {
            None
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
                    Some(json_valid_guard(d, None, col, format!("{ex} {op} '{raw}'")))
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
    /// Whether a JSON path under a column reads the column as JSON
    /// on the strength of its declaration alone: the column step
    /// (the first step) carries no `<json>` trait. Such a column is
    /// an assumption of the plan ([`ColumnTypes::json`]); with the
    /// trait the graft is declared on the step and holds on both
    /// roads.
    fn note_json_column(&mut self, o: NodeId, col: &str) {
        let declared_on_hop = self.kids(o, "step").first().is_some_and(|s| {
            self.kids(*s, "trait").iter().any(|t| {
                matches!(self.prop(*t, "alts"), Some(Value::List(alts))
                    if alts.iter().any(|a| matches!(a, Value::Str(x) if x == "json")))
            })
        });
        if !declared_on_hop && !self.column_types.json.iter().any(|c| c == col) {
            self.column_types.json.push(col.to_string());
        }
        // The child form (the bare `::`, or no projection): what the
        // property reading leaves empty.
        let keyed = self
            .kid(o, "projection")
            .is_some_and(|p| self.prop(p, "key").is_some());
        if !keyed {
            self.column_types.json_children = true;
        }
    }

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
            .ok_or_else(|| not_pushed(Refusal::Name, "empty query".into()))?;
        let mut sel = Select::default();

        // Correlations: exactly one becomes the JOIN's left table.
        let corrs = self.kids(q, "query");
        let branches = self.kids(q, "branch");
        if branches.len() != 1 {
            return Err(not_pushed(
                Refusal::Union,
                "a branch union (SQL has UNION, but result shapes differ; export one branch)"
                    .into(),
            ));
        }

        if corrs.len() > 1 {
            return Err(not_pushed(
                Refusal::Join,
                "more than one correlation context".into(),
            ));
        }
        if let Some(corr) = corrs.first() {
            // Driver-first: the query's own branch is the FROM
            // table (the driver); the correlation entry is the
            // joined table, whose `$$` equalities form the ON.
            let (ltable, lpreds) = self.table_branch(branches[0])?;
            let (rtable, rpreds) = self.table_branch(self.kids(*corr, "branch")[0])?;
            self.from_marks = self.row_marks(branches[0])?;
            self.join_marks = self.row_marks(self.kids(*corr, "branch")[0])?;
            // a joined expression with a pipeline of its own binds
            // what that pipeline leaves: not a row of its table
            if self.kid(*corr, "pipeline").is_some() {
                return Err(not_pushed(
                    Refusal::Join,
                    "a joined expression with a pipeline of its own".into(),
                ));
            }
            // The SQL renderings of the two table names (validated
            // or quoted); the raw names stay in the plan metadata,
            // which the driver matches against its catalog.
            let lsql = self.sql_ident(&ltable, "table name")?;
            let rtable_sql = self.sql_ident(&rtable, "table name")?;
            // A table joined to itself stands in the statement
            // twice, and one of the two needs a name of its own:
            // the joined side is aliased, and everything said of it
            // (the ON, the witness's columns, the second key of the
            // order) is said under the alias. (It was written under
            // the one name, `FROM employees JOIN employees ON
            // employees.manager_id = employees.id`, which every
            // database rejects as ambiguous: such a query always
            // fell back to the scan.)
            self.join_alias = (ltable == rtable).then(|| {
                if is_plain_ident(&rtable) {
                    format!("{rtable}_2")
                } else {
                    "joined_2".to_string()
                }
            });
            let rsql = self
                .join_alias
                .clone()
                .unwrap_or_else(|| rtable_sql.clone());
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
            // `<=>?` — the outer correlation keeps a driver row no
            // joined row admits, its witness null: a LEFT JOIN. The
            // joined expression's own predicates then select the
            // witness, so they belong to the ON; in a WHERE they
            // would discard the unmatched rows again.
            let outer = matches!(self.prop(*corr, "outer"), Some(Value::Bool(true)));
            if outer {
                on.append(&mut wheres);
            }
            sel.join_outer = outer;
            if on.is_empty() {
                return Err(not_pushed(
                    Refusal::Join,
                    "a correlation without a '$$' equality (no JOIN condition)".into(),
                ));
            }
            for p in driver_conds {
                let cond = self.predicate_cond(p, Some(&lsql.clone()))?;
                wheres.push(cond);
            }
            // `<=>*` — every witness: the JOIN as SQL means it, one
            // row per matching pair; the plain `<=>` binds one.
            self.join_every = matches!(self.prop(*corr, "every"), Some(Value::Bool(true)));
            if !self.join_every {
                self.notes.push(
                    "JOIN: Quarb's binding is existential (one row per FROM row, \
                     bound to its first witness); SQL multiplies rows when \
                     several joined rows match"
                        .to_string(),
                );
            }
            sel.from = lsql.clone();
            self.from_table = ltable;
            self.join_table = Some(rtable);
            // (no AS: Oracle takes a table alias without the word)
            let joined = match &self.join_alias {
                Some(alias) => format!("{rtable_sql} {alias}"),
                None => rtable_sql.clone(),
            };
            sel.join = Some((joined, on.join(" AND ")));
            sel.wheres = wheres;
            // A terminal projection on the driving branch is a
            // one-column select of the FROM table.
            let mut projected = None;
            if let Some(proj) = self.kid(branches[0], "projection") {
                let name = self.projection_col(proj)?;
                let col = self.sql_ident(&name, "column name")?;
                let col = format!("{lsql}.{col}");
                sel.select.push(col.clone());
                projected = Some((col, name));
            }
            if self
                .kid(self.kids(*corr, "branch")[0], "projection")
                .is_some()
            {
                return Err(not_pushed(
                    Refusal::Join,
                    "a projection on the joined expression (project the witness \
                     in the pipeline: '$$1::col')"
                        .into(),
                ));
            }
            self.pipeline(q, &mut sel, Some((&lsql, &rsql)), projected)?;
        } else {
            let (table, preds) = self.table_branch(branches[0])?;
            self.from_marks = self.row_marks(branches[0])?;
            sel.from = self.sql_ident(&table, "table name")?;
            self.from_table = table;
            for p in preds {
                let cond = self.predicate_cond(p, None)?;
                sel.wheres.push(cond);
            }
            // A terminal projection is a one-column select.
            let mut projected = None;
            if let Some(proj) = self.kid(branches[0], "projection") {
                let name = self.projection_col(proj)?;
                let col = self.sql_ident(&name, "column name")?;
                sel.select.push(col.clone());
                projected = Some((col, name));
            }
            self.pipeline(q, &mut sel, None, projected)?;
        }
        // A whole plan answers values: a column, a record, a
        // reduction. A query that ends on row nodes is answered by
        // their locators, which are the scan's to write (ruling
        // 2026-11-18); it was pushed as SELECT * and printed each
        // row as a record, where the scan printed `/books/1`. Its
        // conditions still go ahead of the scan through the partial
        // plan, and nothing the reader wrote is refused, so the
        // refusal carries no code.
        if self.strict && sel.select.is_empty() {
            return Err(not_pushed(
                Refusal::Nodes,
                "a selection of row nodes is answered by their locators".into(),
            ));
        }
        Ok(sel.render())
    }

    /// A `/table/*[preds]` branch: the table name and the row-step
    /// predicate nodes.
    fn table_branch(&mut self, b: NodeId) -> Result<(String, Vec<NodeId>), SqlError> {
        self.table_branch_lead(b, false)
    }

    /// [`table_branch`]; with `more`, the branch may go on after
    /// `/table/*[…]` (a step, a cell): the prefilter concerns its
    /// first two steps only, and the steps after them are held to
    /// the whole-query gates.
    fn table_branch_lead(
        &mut self,
        b: NodeId,
        more: bool,
    ) -> Result<(String, Vec<NodeId>), SqlError> {
        let steps = self.kids(b, "step");
        if steps.len() < 2 || (steps.len() > 2 && !more) {
            return Err(not_pushed(
                Refusal::Walk,
                "navigation beyond /table/* (SQL sees tables and rows)".into(),
            ));
        }
        let (t, rows) = (steps[0], steps[1]);
        if self.prop_s(t, "axis") != "/"
            || self.prop_s(t, "matcher-kind") != "name"
            || self.prop_s(rows, "axis") != "/"
            || self.prop_s(rows, "matcher-kind") != "any"
        {
            return Err(not_pushed(
                Refusal::Walk,
                "navigation beyond /table/* (SQL sees tables and rows)".into(),
            ));
        }
        if self.kid(t, "predicate").is_some() {
            return Err(not_pushed(
                Refusal::Walk,
                "a predicate on the table step".into(),
            ));
        }
        Ok((self.prop_s(t, "matcher"), self.kids(rows, "predicate")))
    }

    /// The names a `/table/*` branch marks its rows under: the marks
    /// standing after the row step. A mark anywhere else names a
    /// node SQL has no row for (the table) and is not translated.
    fn row_marks(&self, b: NodeId) -> Result<Vec<String>, SqlError> {
        let kids = self.arbor.children(b);
        let rows = self.kids(b, "step").get(1).copied();
        let after = rows
            .and_then(|r| kids.iter().position(|k| *k == r))
            .unwrap_or(kids.len());
        let mut names = Vec::new();
        for (i, k) in kids.iter().enumerate() {
            if self.kind(*k) != "mark" {
                continue;
            }
            if i < after {
                return Err(not_pushed(
                    Refusal::Walk,
                    "a mark before the rows (SQL sees tables and rows)".into(),
                ));
            }
            // (a mark with no name is read by position alone, and
            // a read by position is not translated)
            if let Some(Value::Str(n)) = self.prop(*k, "name") {
                names.push(n);
            }
        }
        Ok(names)
    }

    /// A path operand read through the capsa's node side (`$$.al`,
    /// `$$.2`, `((@))`): the mark's name where the anchor is one
    /// mark by name, and whether it is any mark anchor at all.
    fn mark_anchor(&self, o: NodeId) -> (Option<String>, bool) {
        let named = match self.prop(o, "mark") {
            Some(Value::Str(n)) => Some(n),
            _ => None,
        };
        let any = named.is_some()
            || [
                "mark-index",
                "mark-top",
                "mark-back",
                "marks-all",
                "marks-name",
                "edge-end",
                "node-id",
            ]
            .iter()
            .any(|k| self.prop(o, k).is_some());
        (named, any)
    }

    /// One row predicate as a WHERE condition (qualify columns with
    /// `qualifier` when joining).
    fn predicate_cond(&mut self, p: NodeId, qual: Option<&str>) -> Result<String, SqlError> {
        if self.prop_s(p, "kind") != "expr" {
            return Err(not_pushed(
                Refusal::Take,
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

    /// A condition beneath a negation inside a join's bracket, in
    /// SQL whose three truth values are the engine's there. The
    /// engine reads a key comparison (one that reads the other side
    /// of the join) as unknown for a null on either side and every
    /// other test as true or false; unknown passes through `!`,
    /// yields to a true under `||` and to a false under `&&`, and
    /// only a true admits the candidate. SQL's AND, OR and NOT keep
    /// the same table over TRUE, FALSE and UNKNOWN, and its
    /// comparison is unknown for a NULL as the key comparison is. So
    /// a key comparison is written as it stands, AND / OR / NOT as
    /// themselves, and every other test is made two-valued (`CASE
    /// WHEN … THEN 1 ELSE 0 END = 1`), since a NOT above it would
    /// otherwise keep SQL's UNKNOWN where the engine has a plain
    /// false to turn.
    fn join_cond3(&mut self, e: NodeId, qual: Option<&str>) -> Result<String, SqlError> {
        let kids = |ex: &mut Self, e: NodeId| -> Result<Vec<String>, SqlError> {
            ex.arbor
                .children(e)
                .into_iter()
                .map(|c| ex.join_cond3(c, qual))
                .collect()
        };
        match self.kind(e).as_str() {
            kind @ ("and" | "or") => {
                let op = kind.to_uppercase();
                Ok(format!("({})", kids(self, e)?.join(&format!(" {op} "))))
            }
            "not" => Ok(format!("NOT ({})", kids(self, e)?.join(" AND "))),
            "parens" => Ok(format!("({})", kids(self, e)?.join(" AND "))),
            "compare" if self.subtree_has_driver(e) => self.pred_expr(e, qual),
            _ => Ok(format!(
                "(CASE WHEN {} THEN 1 ELSE 0 END = 1)",
                self.pred_expr(e, qual)?
            )),
        }
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
                // its negation is true). A pushed plan says the
                // negation the way that keeps the row on every
                // engine: the condition as a CASE, which takes
                // UNKNOWN to its ELSE. Inside a join's bracket the
                // engine's own logic is three-valued for the key
                // comparisons and two-valued for the rest: a
                // negation with no key comparison beneath it is the
                // CASE form like any other, and one over a key
                // comparison is written in SQL's own three values
                // ([`join_cond3`]). (0.70 left both to the engine.)
                //
                // [`join_cond3`]: Self::join_cond3
                if self.strict && self.in_on && self.subtree_has_driver(e) {
                    let inner: Vec<String> = self
                        .arbor
                        .children(e)
                        .into_iter()
                        .map(|c| self.join_cond3(c, qual))
                        .collect::<Result<_, _>>()?;
                    return Ok(format!("NOT ({})", inner.join(" AND ")));
                }
                let inner: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c, qual))
                    .collect::<Result<_, _>>()?;
                if self.strict {
                    return Ok(format!(
                        "(CASE WHEN {} THEN 0 ELSE 1 END = 1)",
                        inner.join(" AND ")
                    ));
                }
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
                    // A comparison with the literal null is the
                    // presence test wherever it stands, inside a join
                    // condition too (ruling 2026-11-18: no key
                    // comparison, true or false): IS [NOT] NULL. In
                    // 0.69 the engine left it unknown there and the
                    // plan was refused.
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
                            self.note_json_column(kids[pi], &col);
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
                                return Err(mismatch(
                                    Refusal::Type,
                                    "a numeric-looking text literal against a \
                                     JSON path compares numerically in Quarb \
                                     (value coercion); no SQL extractor \
                                     matches that"
                                        .into(),
                                ));
                            }
                            // An extractor answers a container as
                            // its JSON text (SQLite `[1,2]`,
                            // PostgreSQL `[1, 2]`), so a literal that
                            // is such a text equalled the array or
                            // the object itself, which has no value
                            // in the graft: the push kept a row the
                            // scan drops. No scalar's text opens
                            // with a bracket or a brace that a
                            // container's does not, so such a
                            // literal rides the prefilter.
                            if raw.starts_with('[') || raw.starts_with('{') {
                                return Err(mismatch(
                                    Refusal::Type,
                                    "a text literal that opens like a JSON container \
                                     could equal an array's or an object's own text \
                                     in the extractor, which has no value in Quarb"
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
                    // The property form, `[/col/a::b = 'lit']`: the
                    // same question on a scalar, and on an array of
                    // scalars the list, which equals the literal when
                    // any item does (an array that holds a container
                    // is no property). SQLite says both in one
                    // condition, exactly: the scalar branch by the
                    // value's type and text, the array branch by
                    // `json_each`. The other engines' array branches
                    // are not verified live, so there the shape rides
                    // the prefilter (PostgreSQL) or the scan.
                    for (pi, li) in [(0usize, 1usize), (1, 0)] {
                        if self.is_text_literal(kids[li])
                            && let Some((col, path)) = self.json_property_path(kids[pi])
                        {
                            self.note_json_column(kids[pi], &col);
                            let raw = self
                                .prop(kids[li], "value")
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            if raw.trim().parse::<f64>().is_ok() {
                                return Err(mismatch(
                                    Refusal::Type,
                                    "a numeric-looking text literal against a \
                                     JSON path compares numerically in Quarb \
                                     (value coercion); no SQL extractor \
                                     matches that"
                                        .into(),
                                ));
                            }
                            // `json_each` names its own columns; an
                            // unqualified JSON column of one of those
                            // names would read as them inside it.
                            let shadowed = qual.is_none()
                                && JSON_EACH_COLUMNS.contains(&col.to_ascii_lowercase().as_str());
                            if dialect != Dialect::Sqlite || shadowed {
                                return Err(mismatch(
                                    Refusal::Type,
                                    "a property of a JSON value may be a list (an \
                                     array of scalars equals the literal when any \
                                     item does); only SQLite's condition carries \
                                     that branch whole; this shape rides the \
                                     partial prefilter"
                                        .into(),
                                ));
                            }
                            let lit = self.operand(kids[li], qual)?;
                            let qcol = match qual {
                                Some(q) => format!("{q}.{col}"),
                                None => col.clone(),
                            };
                            let at = format!("'$.{}'", path.join("."));
                            let ty = format!("json_type({qcol}, {at})");
                            let ex = json_extract(dialect, qual, &col, &path);
                            return Ok(json_valid_guard(
                                dialect,
                                qual,
                                &col,
                                format!(
                                    "(({ty} = 'text' AND {ex} = {lit}) OR ({ty} = 'array' \
                                     AND EXISTS (SELECT 1 FROM json_each({qcol}, {at}) \
                                     WHERE json_each.type = 'text' AND json_each.value = {lit}) \
                                     AND NOT EXISTS (SELECT 1 FROM json_each({qcol}, {at}) \
                                     WHERE json_each.type IN ('array', 'object'))))"
                                ),
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
                            || self.json_property_path(side).is_some()
                        {
                            return Err(mismatch(
                                Refusal::Type,
                                "a JSON-path comparison pushes whole only as \
                                 fixed-path string equality (Quarb coerces \
                                 numeric-looking values; no extractor matches \
                                 that); this shape rides the partial prefilter"
                                    .into(),
                            ));
                        }
                    }
                }
                // Quarb compares by value; SQL by the column's type.
                // A column against a numeric literal is the scan's
                // comparison only on a numeric-typed column, and
                // against a text literal only on a column that is
                // not (MySQL reads `n = 'abc'` as `n = 0`): the
                // catalog decides, so both are recorded for the
                // caller. A numeric-looking TEXT literal meets
                // numbers as a number in Quarb and compares as text
                // in SQL, on every engine, so it does not push. A
                // regex is no value: its refusal below is the
                // pattern's, whatever its text reads as.
                if self.strict && !matches!(op.as_str(), "==" | "!==") {
                    // Two values of the row compared with each other
                    // (`[::a = ::b]`, a join's `::album_id =
                    // $$0::id`, `[::n + 1 > ::m]`): the engine
                    // compares by value, so two texts that read as
                    // numbers compare as numbers ('1.0' equals '1',
                    // '10' is above '9'), and SQL compares two text
                    // columns as texts. The two agree where every
                    // column compared is numeric: the catalog
                    // decides. (They were pushed unchecked: over
                    // text columns the plan answered other rows.)
                    let value =
                        |k: NodeId| matches!(self.kind(k).as_str(), "path" | "context" | "arith");
                    if value(kids[0]) && value(kids[1]) {
                        for k in [kids[0], kids[1]] {
                            let column = self.column_operand(k);
                            self.assume_numeric(column);
                        }
                        self.column_types.columns_compared = true;
                    }
                    // A text literal against anything but a plain
                    // column: a computed value is a number or
                    // nothing, and SQL orders a number below any
                    // text where the engine says the two never meet.
                    for (ci, li) in [(0usize, 1usize), (1, 0)] {
                        if self.is_text_literal(kids[li]) && self.kind(kids[ci]) == "arith" {
                            return Err(mismatch(
                                Refusal::Type,
                                "a text literal compared with a computed value (SQL orders a \
                                 number below every text; to the engine the two never meet)"
                                    .into(),
                            ));
                        }
                    }
                    for (ci, li) in [(0usize, 1usize), (1, 0)] {
                        let Some(column) = self.column_operand(kids[ci]) else {
                            continue;
                        };
                        if self.kind(kids[li]) != "literal" {
                            continue;
                        }
                        let numeric = self
                            .prop(kids[li], "value")
                            .is_some_and(|v| Value::Str(v.to_string()).numeric_reading().is_some());
                        let list = match (self.prop_s(kids[li], "type").as_str(), numeric) {
                            ("null", _) => continue,
                            ("text", true) => {
                                return Err(mismatch(
                                    Refusal::Type,
                                    "a numeric-looking text literal compares \
                                     numerically in Quarb (value coercion); SQL \
                                     compares it as text"
                                        .into(),
                                ));
                            }
                            ("text", false) => &mut self.column_types.text,
                            (_, true) => &mut self.column_types.numeric,
                            (_, false) => continue,
                        };
                        if !list.contains(&column) {
                            list.push(column);
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
                        return Err(not_pushed(
                            Refusal::Pattern,
                            format!("a pattern literal on '{op}' (only '=' pushes)"),
                        ));
                    }
                    // PostgreSQL's LIKE is case-sensitive under its
                    // deterministic collations — provably what the
                    // pattern test does — so the strict push is
                    // sound there (and a prefix pattern is
                    // index-sargable server-side). Everywhere else
                    // case folding is ambient state (SQLite pragma,
                    // MySQL/MSSQL collation), so strict refuses.
                    if self.strict && self.dialect != Some(Dialect::Postgres) {
                        return Err(mismatch(
                            Refusal::Pattern,
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
                    // LIKE is the pattern's test over a column whose
                    // text PostgreSQL compares as it stands (a
                    // `char(n)` is read without its padding there, a
                    // `citext` without its case): the catalog says
                    let column = self.column_operand(kids[0]);
                    self.assume_text(column);
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
                // Which sides can be NULL: every side that is no
                // literal. And whether this is a key comparison of a
                // join condition, which the engine leaves unknown for
                // a null on either side, as SQL does: there the plain
                // operators are the engine's own.
                let nullable: Vec<&str> = [(kids[0], l.as_str()), (kids[1], r.as_str())]
                    .into_iter()
                    .filter(|(k, _)| self.kind(*k) != "literal")
                    .map(|(_, sql)| sql)
                    .collect();
                let keyed = self.in_on
                    && (self.subtree_has_driver(kids[0]) || self.subtree_has_driver(kids[1]));
                // A text column against a text literal. The engine
                // compares two texts by their characters: case,
                // accents and trailing spaces all tell, and an order
                // is the order of their UTF-8 bytes. A database
                // compares under the column's collation, and what
                // that is differs by engine:
                // - MySQL and MariaDB fold case and accents and pad
                //   with spaces by default ('holst' met 'Holst',
                //   'Holst ' and 'hölst'), so the comparison is
                //   written over the UTF-8 bytes of both sides, with
                //   the collation's own equality kept beside an `=`
                //   for the index it can use;
                // - PostgreSQL's `=` is exact under its deterministic
                //   collations and its order is the collation's: an
                //   order is written under "C", the order of bytes;
                // - SQL Server folds case by default and no server
                //   stood here to hold another form against: the
                //   comparison stays with the engine there (an `=`
                //   still narrows the fetch, through the partial
                //   plan);
                // - SQLite compares bytes unless the column is
                //   declared under a collation, which its catalog
                //   reports; DuckDB compares bytes.
                let text_pair = self.strict
                    && ((self.is_text_literal(kids[1]) && self.column_operand(kids[0]).is_some())
                        || (self.is_text_literal(kids[0])
                            && self.column_operand(kids[1]).is_some()));
                if text_pair && matches!(op.as_str(), "=" | "!=" | "<" | "<=" | ">" | ">=") {
                    match self.dialect {
                        Some(Dialect::Mssql) => {
                            return Err(mismatch(
                                Refusal::Type,
                                "a text comparison follows the database's collation on SQL \
                                 Server (case, accents and trailing spaces), where the engine \
                                 compares the characters"
                                    .into(),
                            ));
                        }
                        Some(Dialect::MySql) => {
                            let bytes = |s: &str| format!("BINARY CONVERT({s} USING utf8mb4)");
                            let (bl, br) = (bytes(&l), bytes(&r));
                            return Ok(match op.as_str() {
                                "=" => format!("({l} = {r} AND {bl} = {br})"),
                                // (a join's key comparison stays
                                // unknown for a null, as SQL's is)
                                "!=" if keyed => format!("{bl} <> {br}"),
                                "!=" => {
                                    let column = nullable.first().copied().unwrap_or(l.as_str());
                                    format!("({bl} <> {br} OR {column} IS NULL)")
                                }
                                order => format!("{bl} {order} {br}"),
                            });
                        }
                        Some(Dialect::Postgres) if !matches!(op.as_str(), "=" | "!=") => {
                            return Ok(format!("{l} {op} {r} COLLATE \"C\""));
                        }
                        _ => {}
                    }
                }
                Ok(match op.as_str() {
                    // Two NULLs are equal in Quarb (`value_eq`), and
                    // SQL's `=` is UNKNOWN for them: an equality of
                    // two columns says the second case too. (It was
                    // pushed as the bare `a = b`, which dropped the
                    // row where both are NULL.)
                    "=" if self.strict && !keyed && nullable.len() == 2 => {
                        format!("({l} = {r} OR ({l} IS NULL AND {r} IS NULL))")
                    }
                    "=" => format!("{l} = {r}"),
                    // Quarb's `!=` holds for a NULL against a value
                    // and fails for two NULLs; SQL's `<>` is UNKNOWN
                    // for either. The plan adds the cases `<>` leaves
                    // out, in SQL every engine reads.
                    "!=" if self.strict && !keyed => match nullable.as_slice() {
                        [] => format!("{l} <> {r}"),
                        [one] => format!("({l} <> {r} OR {one} IS NULL)"),
                        _ => format!(
                            "({l} <> {r} OR ({l} IS NULL AND {r} IS NOT NULL) OR ({l} IS NOT \
                             NULL AND {r} IS NULL))"
                        ),
                    },
                    "!=" if self.strict => format!("{l} <> {r}"),
                    "!=" => {
                        // The display translation keeps `<>` and
                        // notes the divergence.
                        self.notes.push(
                            "'!=' → '<>': Quarb keeps rows whose column is NULL; \
                             SQL's '<>' drops them (use '!= null' for IS NOT NULL)"
                                .to_string(),
                        );
                        format!("{l} <> {r}")
                    }
                    "<" | "<=" | ">" | ">=" => format!("{l} {op} {r}"),
                    // SQLite's instr is a substring test by bytes,
                    // case and all, as `*=` is; the column is a text
                    // one (recorded above with the literal: a number
                    // would be searched in SQLite's rendering of it,
                    // not the engine's). A NULL has no substring on
                    // either side.
                    "*=" if self.strict
                        && self.dialect == Some(Dialect::Sqlite)
                        && self.is_text_literal(kids[1])
                        && self.column_operand(kids[0]).is_some() =>
                    {
                        format!("instr({l}, {r}) > 0")
                    }
                    "*=" => {
                        // The same PostgreSQL rung as the pattern
                        // literal above: LIKE is provably
                        // case-sensitive there, so `*=` pushes
                        // strict too.
                        if self.strict && self.dialect != Some(Dialect::Postgres) {
                            return Err(mismatch(
                                Refusal::Pattern,
                                "LIKE case folding differs per engine \
                                 (a substring test is pushed on SQLite as instr and on \
                                 PostgreSQL as LIKE)"
                                    .into(),
                            ));
                        }
                        // The pattern must be a text literal: a
                        // column or computed operand holds a value,
                        // not a pattern, and SQL LIKE cannot express
                        // "contains that value" portably.
                        if !self.is_text_literal(kids[1]) {
                            return Err(not_pushed(
                                Refusal::Pattern,
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
                    "==" | "!==" | "=~" | "!~" => {
                        return Err(mismatch(
                            Refusal::Pattern,
                            "regex matching means a different REGEXP engine per \
                             backend; regexes run engine-side"
                                .into(),
                        ));
                    }
                    other => {
                        return Err(not_pushed(
                            Refusal::Operator,
                            format!("the '{other}' comparison"),
                        ));
                    }
                })
            }
            // A bare truthy operand.
            _ => {
                if self.strict {
                    return Err(mismatch(
                        Refusal::Type,
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
                            return Err(mismatch(
                                Refusal::Name,
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
                        return Err(not_pushed(
                            Refusal::Resolution,
                            "a '-->' resolution chain: the foreign-key targets live in \
                             the schema, not the query — spell the join with '<=>' to \
                             export it"
                                .into(),
                        ));
                    }
                    return Err(not_pushed(
                        Refusal::Resolution,
                        "navigation inside a predicate (SQL rows are flat)".into(),
                    ));
                }
                let p = self
                    .kid(o, "projection")
                    .ok_or_else(|| not_pushed(Refusal::Operator, "an empty path operand".into()))?;
                let col = self.projection_col(p)?;
                let col = self.sql_ident(&col, "column name")?;
                // a read through a mark: the row the mark names
                let (named, any) = self.mark_anchor(o);
                if any {
                    let Some(name) = named else {
                        return Err(not_pushed(
                            Refusal::State,
                            "a mark read by position, or of several marks".into(),
                        ));
                    };
                    if self.join_marks.contains(&name) {
                        // `$$.al::col`: the witness, as `$$1::col`
                        if self.in_on {
                            return Err(not_pushed(
                                Refusal::Join,
                                "the joined expression's own mark inside its ON".into(),
                            ));
                        }
                        let Some(join) = self.join_sql.clone() else {
                            return Err(not_pushed(
                                Refusal::Join,
                                "a join mark outside a correlation join".into(),
                            ));
                        };
                        return Ok(format!("{join}.{col}"));
                    }
                    if self.from_marks.contains(&name) {
                        // `$$.t::col`: the driver's row, as `$$0::col`
                        // in the ON
                        return Ok(if self.join_sql.is_some() {
                            format!("{}.{col}", self.from_sql.clone())
                        } else {
                            match qual {
                                Some(q) => format!("{q}.{col}"),
                                None => col,
                            }
                        });
                    }
                    return Err(not_pushed(
                        Refusal::State,
                        format!("a read of the mark '.{name}' (not a row of the query's tables)"),
                    ));
                }
                Ok(match qual {
                    Some(q) => format!("{q}.{col}"),
                    None => col,
                })
            }
            "context" => {
                // `$$1::col` — the joined expression's witness, in
                // pipeline position: the joined table's column.
                // Anything else is outside the verified-safe set.
                let p = self
                    .kid(o, "projection")
                    .ok_or_else(|| not_pushed(Refusal::Join, "a bare '$$N' reference".into()))?;
                let col = self.projection_col(p)?;
                let col = self.sql_ident(&col, "column name")?;
                if matches!(self.prop(o, "index"), Some(Value::Int(0))) {
                    // `$$0::col` — the driver, legal only inside the
                    // joined expression's ON bracket (the parser
                    // refuses it anywhere else).
                    if !self.in_on {
                        return Err(not_pushed(
                            Refusal::Join,
                            "a '$$0' reference outside the join's ON".into(),
                        ));
                    }
                    if self.kid(o, "step").is_some() {
                        return Err(not_pushed(
                            Refusal::Join,
                            "a '$$0' reference beyond a plain column ($$0::col)".into(),
                        ));
                    }
                    return Ok(format!("{}.{col}", self.from_sql.clone()));
                }
                if self.in_on {
                    return Err(not_pushed(
                        Refusal::Join,
                        "a '$$N' reference inside the ON (the driver is '$$0')".into(),
                    ));
                }
                let Some(join) = self.join_sql.clone() else {
                    return Err(not_pushed(
                        Refusal::Join,
                        "a '$$N' reference outside a correlation join".into(),
                    ));
                };
                match self.prop(o, "index") {
                    Some(Value::Int(1)) => Ok(format!("{join}.{col}")),
                    None => Err(not_pushed(Refusal::Join, "a bare '$$N' reference".into())),
                    Some(v) => Err(not_pushed(
                        Refusal::Join,
                        format!("$${v} beyond a two-branch correlation"),
                    )),
                }
            }
            "invoking" => {
                // `$$__::col` — the invoking position of an enclosing
                // query body, never the driver (that is `$$0`, a
                // context of index 0, since draft 153). SQL has no
                // enclosing body to read.
                Err(not_pushed(
                    Refusal::Join,
                    "a '$$__' reference (the driver inside the ON is '$$0')".into(),
                ))
            }
            "arith" => {
                let op = self.prop_s(o, "op");
                let kids = self.arbor.children(o);
                // The engine computes with values: a text that reads
                // as a number is one, any other text gives null. SQL
                // computes by the column's type (SQLite reads 'abc'
                // as 0, so `::a + 1 = 1` answered such rows). A
                // column computed with is a numeric one, by the
                // catalog; a text literal is not computed with.
                if self.strict {
                    for &k in &kids {
                        if self.is_text_literal(k) {
                            return Err(mismatch(
                                Refusal::Type,
                                "arithmetic with a text literal (the engine reads the number \
                                 it spells; SQL's reading differs per engine)"
                                    .into(),
                            ));
                        }
                        let column = self.column_operand(k);
                        if let Some(c) = &column
                            && self.strict
                            && !self.column_types.computed.contains(c)
                        {
                            self.column_types.computed.push(c.clone());
                        }
                        self.assume_numeric(column);
                    }
                }
                // `div` is the true division: 7 div 2 is 3.5, and a
                // division by zero answers inf or nan. SQL's `/`
                // divides two integers as integers on most engines,
                // and answers NULL or an error for a zero divisor.
                // So the plan divides only by a number written in
                // the query that is not zero, with the dividend
                // read as a float. (It was pushed as the bare `/`:
                // `::n div 4` answered 2 where the scan answers 2.5.)
                if self.strict && op == "div" {
                    let nonzero = self.kind(kids[1]) == "literal"
                        && self
                            .prop(kids[1], "value")
                            .and_then(|v| Value::Str(v.to_string()).numeric_reading())
                            .is_some_and(|n| {
                                !matches!(n, Value::Int(0))
                                    && !matches!(n, Value::Float(f) if f == 0.0)
                            });
                    if !nonzero {
                        return Err(mismatch(
                            Refusal::Operator,
                            "'div' by anything but a number other than zero (for a zero \
                             divisor the engine answers inf or nan, SQL no value or an error)"
                                .into(),
                        ));
                    }
                    let l = self.operand(kids[0], qual)?;
                    let r = self.operand(kids[1], qual)?;
                    let float = match self.dialect {
                        Some(Dialect::Sqlite) => "REAL",
                        Some(Dialect::Postgres) => "DOUBLE PRECISION",
                        Some(Dialect::Mssql) => "FLOAT",
                        Some(Dialect::Oracle) => "BINARY_DOUBLE",
                        Some(Dialect::MySql) | None => "DOUBLE",
                    };
                    return Ok(format!("(CAST({l} AS {float}) / {r})"));
                }
                // `mod` keeps a fraction (9.5 mod 3 is 0.5) and
                // answers nan for a zero divisor; SQL's `%` reads
                // integers and answers NULL or an error.
                if self.strict && op == "mod" {
                    return Err(mismatch(
                        Refusal::Operator,
                        "'mod' (SQL's % reads its operands as integers, and answers no value \
                         for a zero divisor)"
                            .into(),
                    ));
                }
                let l = self.operand(kids[0], qual)?;
                let r = self.operand(kids[1], qual)?;
                Ok(match op.as_str() {
                    "+" | "-" | "*" => format!("({l} {op} {r})"),
                    "div" => format!("({l} / {r})"),
                    "mod" => format!("({l} % {r})"),
                    other => {
                        return Err(not_pushed(
                            Refusal::Operator,
                            format!("'{other}' arithmetic"),
                        ));
                    }
                })
            }
            other => Err(not_pushed(
                Refusal::State,
                format!(
                    "the '{other}' operand (registers, topics, and captures are Quarb-side state)"
                ),
            )),
        }
    }

    fn projection_col(&mut self, p: NodeId) -> Result<String, SqlError> {
        match self.prop_s(p, "kind").as_str() {
            "property" => match self.prop(p, "key") {
                Some(k) => Ok(k.to_string()),
                None => Err(not_pushed(
                    Refusal::Operator,
                    "the bare '::' projection (name the column)".into(),
                )),
            },
            other => Err(not_pushed(
                Refusal::Operator,
                format!("the {other} metadata projection"),
            )),
        }
    }

    /// The column a plain column operand names (`::col`, `$*1::col`):
    /// a path without a step, or a context reference, projecting a
    /// named property.
    /// A numeric reducer (`sum`, `mean`, `min`, `max`) pushed over a
    /// column is the scan's only on a numeric-typed column: the
    /// scan lifts unit text (`'18 parasang'` under a declared unit)
    /// to quantities and orders texts as texts, where SQL sums a
    /// text column by its leading digits and orders it by
    /// collation. Recorded for the caller's catalog as a
    /// comparison's assumption is.
    /// The column a reducer folds: assumed numeric, and noted where
    /// the reducer is a sum or a mean over a column by its name
    /// ([`FloatSum`]).
    fn fold_column(
        &mut self,
        reducer: &str,
        name: &mut Option<String>,
        col: &mut Option<String>,
    ) -> Option<String> {
        let column = name.clone();
        if let Some(c) = &column
            && self.strict
            && !self.column_types.reduced.contains(c)
        {
            self.column_types.reduced.push(c.clone());
        }
        self.assume_numeric(name.take());
        let sql = col.take();
        if let (true, Some(column), Some(sql)) =
            (matches!(reducer, "sum" | "mean"), column, sql.as_ref())
        {
            self.sums.push((reducer == "mean", column, sql.clone()));
        }
        sql
    }

    fn assume_numeric(&mut self, column: Option<String>) {
        if !self.strict {
            return;
        }
        if let Some(c) = column
            && !self.column_types.numeric.contains(&c)
        {
            self.column_types.numeric.push(c);
        }
    }

    /// A column whose text the plan compares as the engine does
    /// only where the database compares it exactly: recorded for the
    /// caller's catalog, as a text literal's column is.
    fn assume_text(&mut self, column: Option<String>) {
        if !self.strict {
            return;
        }
        if let Some(c) = column
            && !self.column_types.text.contains(&c)
        {
            self.column_types.text.push(c);
        }
    }

    fn column_operand(&self, o: NodeId) -> Option<String> {
        match self.kind(o).as_str() {
            "path" if self.kid(o, "step").is_none() => {}
            "context" => {}
            _ => return None,
        }
        let p = self.kid(o, "projection")?;
        if self.prop_s(p, "kind") != "property" {
            return None;
        }
        self.prop(p, "key").map(|k| k.to_string())
    }

    /// Whether `o` is a text string literal (`'London'`).
    fn is_text_literal(&self, o: NodeId) -> bool {
        self.kind(o) == "literal" && self.prop_s(o, "type") == "text"
    }

    /// If `o` is the one JSON-column-path shape pushdown handles —
    /// `/col/seg/seg…::`, a plain-navigation path (every step `/`
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
        // the JSON leaf), not `::key` or a `:::`/`:` metadata form.
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

    /// The property form of a JSON-column path, `/col/seg…::key`:
    /// plain-navigation steps as in [`json_path`], at least the
    /// column's own, ending in the projection of a plain-identifier
    /// property. It asks for the same value as `/col/seg…/key::`
    /// where that is a scalar; on an array of scalars it is the
    /// list (see [`json_property_prefilter`]). Returns
    /// `(column, [json segments, the key last])`.
    ///
    /// [`json_path`]: Self::json_path
    /// [`json_property_prefilter`]: Self::json_property_prefilter
    fn json_property_path(&self, o: NodeId) -> Option<(String, Vec<String>)> {
        if self.kind(o) != "path" {
            return None;
        }
        let steps = self.kids(o, "step");
        if steps.is_empty() {
            return None;
        }
        let proj = self.kid(o, "projection")?;
        if self.prop_s(proj, "kind") != "property" {
            return None;
        }
        let key = self.prop(proj, "key")?.to_string();
        let plain = |name: &str| {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if !plain(&key) {
            return None;
        }
        let mut names = Vec::with_capacity(steps.len() + 1);
        for s in &steps {
            if self.prop_s(*s, "axis") != "/"
                || self.prop_s(*s, "matcher-kind") != "name"
                || self.kid(*s, "predicate").is_some()
            {
                return None;
            }
            let name = self.prop_s(*s, "matcher");
            if !plain(&name) {
                return None;
            }
            names.push(name);
        }
        names.push(key);
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
            return Err(not_pushed(
                Refusal::Name,
                format!(
                    "field name {name:?} needs SQL quoting \
                 (dialects disagree); rename the field"
                ),
            ));
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
            return Err(not_pushed(
                Refusal::Name,
                format!("{what} {name:?} is not a plain SQL identifier"),
            ));
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
            return Err(not_pushed(
                Refusal::Take,
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
            let uses_driver = self.subtree_has_driver(e);
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
        // The `$$0` side is the driver — not part of the joined
        // table's obligation.
        if self.is_driver(e) {
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

    /// `$$0`: a context operand of index 0 (the driver), or a read
    /// of the driver's row through its mark (`$$.t::col`).
    fn is_driver(&self, n: NodeId) -> bool {
        (self.kind(n) == "context" && matches!(self.prop(n, "index"), Some(Value::Int(0))))
            || (self.kind(n) == "path"
                && matches!(self.mark_anchor(n).0, Some(name) if self.from_marks.contains(&name)))
    }

    fn subtree_has_driver(&self, n: NodeId) -> bool {
        self.is_driver(n)
            || self
                .arbor
                .children(n)
                .into_iter()
                .any(|c| self.subtree_has_driver(c))
    }

    /// Consume the pipeline into SELECT clauses. `projected` is the
    /// driving path's own terminal projection, where it has one:
    /// the column as SQL and its name.
    fn pipeline(
        &mut self,
        q: NodeId,
        sel: &mut Select,
        join: Option<(&str, &str)>,
        projected: Option<(String, String)>,
    ) -> Result<(), SqlError> {
        let Some(pipe) = self.kid(q, "pipeline") else {
            return Ok(());
        };
        let commands: Vec<NodeId> = self.arbor.children(pipe);
        let mut i = 0;
        // A pending per-row value (`| ::col`) feeding an aggregate,
        // and the column it names where it is one. The path's own
        // projection is that value from the start, so the glued
        // spelling plans as the piped one: `/t/*::col @| max` is
        // `/t/* | ::col @| max` (it was refused as a reducer "over
        // row nodes", with a hint to write the piped form).
        let (mut pending_col, mut pending_name) = match projected {
            Some((col, name)) => (Some(col), Some(name)),
            None => (None, None),
        };
        // After a grouped reduction: (key, agg_sql, alias).
        let mut grouped: Option<(String, String, String)> = None;
        // A pushed key awaiting its fold: (the regula's name, key_sql).
        let mut pushed_key: Option<(String, String)> = None;
        // …and the column it is, where it is one plain column.
        let mut pushed_key_column: Option<String> = None;
        // The regula a fold went by: the name its key has in the
        // record `%.` answers.
        let mut grouped_reg: Option<String> = None;

        while i < commands.len() {
            let s = commands[i];
            match self.kind(s).as_str() {
                "expr" => {
                    let kids = self.arbor.children(s);
                    pending_col = Some(self.operand(kids[0], join.map(|(l, _)| l))?);
                    pending_name = self.column_operand(kids[0]);
                }
                "cmd" => {
                    let name = self.prop_s(s, "name");
                    match name.as_str() {
                        "rec" | "record" => {
                            sel.select = self.record_fields(s, join)?;
                            self.order.record = true;
                        }
                        // `| %` on a row node is the record of its
                        // properties, which are the row's columns in
                        // the table's order: SELECT *, a record even
                        // of one field. It is the plan a bare
                        // selection of row nodes had before the
                        // ruling of 2026-11-18 gave that one its
                        // locators. Last in the pipeline and over
                        // the rows themselves: after a projection
                        // the thread holds a value, and a joined
                        // row's record is the driver's alone.
                        "%" if self.strict
                            && join.is_none()
                            && sel.select.is_empty()
                            && pending_col.is_none()
                            && grouped.is_none()
                            && pushed_key.is_none()
                            && !self.aggregate
                            && self.arbor.children(s).is_empty()
                            && i + 1 == commands.len() =>
                        {
                            sel.select = vec!["*".to_string()];
                            self.order.record = true;
                            self.order.properties = true;
                        }
                        // A reducing aggregate on the plain pipe:
                        // the grouped reduction.
                        "count" | "sum" | "mean" | "min" | "max" => {
                            let (key, _, _) = grouped.as_ref().ok_or_else(|| {
                                not_pushed(
                                    Refusal::Reducer,
                                    format!("'| {name}' outside a group (use '@| {name}')"),
                                )
                            })?;
                            // Quarb's count counts every member,
                            // nulls included: COUNT(*); the
                            // NULL-skipping COUNT(col) is
                            // `count(--values)`.
                            let values =
                                self.reducer_counts_values(s, &name, pending_col.is_some())?;
                            let col = if name == "count" {
                                pending_col.take().filter(|_| values)
                            } else {
                                self.fold_column(&name, &mut pending_name, &mut pending_col)
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
                                return Err(mismatch(
                                    if other == "unique" {
                                        Refusal::Unique
                                    } else {
                                        Refusal::Sort
                                    },
                                    format!(
                                        "'{other}' means the backend's collation and \
                                     duplicate semantics; one engine-side ordering \
                                     keeps it identical everywhere"
                                    ),
                                ));
                            }
                            return Err(not_pushed(
                                Refusal::Operator,
                                format!("the '{other}' pipeline command"),
                            ));
                        }
                    }
                }
                "push" => {
                    // An alias for the grouped aggregate.
                    if let Some((k, agg, _)) = grouped.take() {
                        let alias = self.prop_s(s, "name");
                        grouped = Some((k, agg, alias));
                    } else {
                        return Err(not_pushed(
                            Refusal::State,
                            "a register push (Quarb-side state)".into(),
                        ));
                    }
                }
                // `| .k(::k)` before a fold by `.k`: the GROUP BY key
                // (a projection body is a subcontext of one branch).
                "expr-push" if grouped.is_none() && pushed_key.is_none() => {
                    let kids = self.arbor.children(s);
                    let key = self.operand(kids[0], join.map(|(l, _)| l))?;
                    pushed_key = Some((self.prop_s(s, "name"), key));
                    pushed_key_column = self.column_operand(kids[0]);
                }
                "subcontext" if grouped.is_none() && pushed_key.is_none() => {
                    let key = self.pushed_column(s, join.map(|(l, _)| l))?;
                    // (unqualified, the SQL is the column's own name)
                    pushed_key_column = join.is_none().then(|| key.clone());
                    pushed_key = Some((self.prop_s(s, "name"), key));
                }
                "filter" => {
                    if grouped.is_some() {
                        // HAVING: `$_` / the alias refer to the
                        // aggregate.
                        let cond = self.having_cond(s)?;
                        sel.having = Some(cond);
                    } else {
                        return Err(not_pushed(
                            Refusal::State,
                            "a mid-pipeline filter (put it in the row predicate)".into(),
                        ));
                    }
                }
                "recall" => {
                    // `| %.` finalizes the grouped record.
                    if self.prop_s(s, "ref") != "%." {
                        return Err(not_pushed(
                            Refusal::State,
                            "a register recall (Quarb-side state)".into(),
                        ));
                    }
                    let (key, agg, alias) = grouped.clone().ok_or_else(|| {
                        not_pushed(Refusal::Fold, "'%.', with nothing grouped".into())
                    })?;
                    let alias = self.alias(&alias)?;
                    // The record names the key by its regula (`.a`),
                    // which need not be the column's name: the scan
                    // answers %(a = …), and so must the plan.
                    let key_field = match &grouped_reg {
                        Some(reg) if *reg != key => format!("{key} AS {}", self.alias(reg)?),
                        _ => key.clone(),
                    };
                    sel.select = vec![key_field, format!("{agg} AS {alias}")];
                    self.order.record = true;
                    // The HAVING condition compared the aggregate
                    // through $_ — substitute the real expression.
                    if let Some(h) = sel.having.take() {
                        sel.having = Some(h.replace("__AGG__", &agg));
                    }
                }
                "agg" if self.prop(s, "group").is_some() => {
                    // The plan takes its first rows last: a command
                    // over the rows a selection kept is the engine's.
                    if self.strict && self.order.limit.is_some() {
                        return Err(not_pushed(
                            Refusal::Take,
                            format!(
                                "the '{}' command after a positional selection (a plan \
                                 takes its first rows last)",
                                self.prop_s(s, "name")
                            ),
                        ));
                    }
                    // `@|{.k} agg`: the fold by the pushed key is the
                    // grouped reduction, GROUP BY key.
                    let name = self.prop_s(s, "name");
                    if !matches!(name.as_str(), "count" | "sum" | "mean" | "min" | "max") {
                        return Err(not_pushed(
                            Refusal::Fold,
                            format!("the '{name}' command per group (GROUP BY reduces)"),
                        ));
                    }
                    let selector = self.prop_s(s, "group");
                    if selector.contains(';') {
                        return Err(not_pushed(
                            Refusal::Fold,
                            "a fold by several regulae (one GROUP BY key translates)".into(),
                        ));
                    }
                    let (reg, key) = pushed_key.take().ok_or_else(|| {
                        not_pushed(
                            Refusal::Fold,
                            "a fold by a regula no push of a key made".into(),
                        )
                    })?;
                    grouped_reg = Some(reg.clone());
                    if selector != "." && selector != format!(".{reg}") {
                        return Err(not_pushed(
                            Refusal::Fold,
                            format!("a fold by '{selector}' (the pushed key is '.{reg}')"),
                        ));
                    }
                    if self.strict {
                        // SQL gives its groups in no stated order; the
                        // engine gives them as each first appears. A
                        // fold by one numeric column of the table is
                        // pushed with that order asked of the driver
                        // (the least key of each group). Numeric: two
                        // texts that read as one number are one key
                        // to the engine and two to GROUP BY.
                        let plain = join.is_none()
                            && self.order.sort.is_none()
                            && self.order.limit.is_none()
                            && !self.order.reversed;
                        match pushed_key_column.take() {
                            Some(column) if plain => {
                                self.assume_numeric(Some(column.clone()));
                                self.order.groups = true;
                                self.order.keyed_by = Some((Refusal::Fold, column));
                            }
                            _ => {
                                return Err(mismatch(
                                    Refusal::Fold,
                                    "a fold over a join, or after a sort or a selection, or by \
                                     anything but one column"
                                        .into(),
                                ));
                            }
                        }
                    }
                    sel.group_by = Some(key.clone());
                    let values = self.reducer_counts_values(s, &name, pending_col.is_some())?;
                    let col = if name == "count" {
                        // `count(--values)` over a projected column is
                        // COUNT(col); the bare count is every row
                        pending_col.take().filter(|_| values)
                    } else {
                        self.fold_column(&name, &mut pending_name, &mut pending_col)
                    };
                    let agg = sql_agg(&name, col)?;
                    grouped = Some((key, agg.clone(), agg));
                }
                "agg" => {
                    // The plan takes its first rows last: a command
                    // over the rows a selection kept is the engine's.
                    if self.strict && self.order.limit.is_some() {
                        return Err(not_pushed(
                            Refusal::Take,
                            format!(
                                "the '{}' command after a positional selection (a plan \
                                 takes its first rows last)",
                                self.prop_s(s, "name")
                            ),
                        ));
                    }
                    let name = self.prop_s(s, "name");
                    match name.as_str() {
                        "count" | "sum" | "mean" | "min" | "max" => {
                            // Quarb's count counts every row, nulls
                            // included: COUNT(*) regardless of a
                            // pending column. `count(--values)`
                            // counts the values other than null:
                            // COUNT(col) over a projected column.
                            let values =
                                self.reducer_counts_values(s, &name, pending_col.is_some())?;
                            let col = if name == "count" {
                                pending_col.take().filter(|_| values)
                            } else {
                                self.fold_column(&name, &mut pending_name, &mut pending_col)
                            };
                            sel.select = vec![sql_agg(&name, col)?];
                            self.aggregate = true;
                        }
                        "sort_by" if self.strict => {
                            // A database orders text by its own
                            // collation. A sort by one numeric column
                            // of the table has one order everywhere:
                            // it is pushed, the key recorded for the
                            // catalog to confirm, a null key last and
                            // the ties in the table's order, which the
                            // driver's ORDER BY says after the plan's.
                            let kids = self.arbor.children(s);
                            let column = kids.first().and_then(|&k| self.column_operand(k));
                            let plain = join.is_none()
                                && self.order.sort.is_none()
                                && self.order.limit.is_none()
                                && !self.order.reversed
                                && !self.order.groups
                                && !self.aggregate
                                && grouped.is_none();
                            let column = match (column, plain) {
                                (Some(column), true) => column,
                                // one column, and no place for its
                                // order in the plan
                                (Some(_), false) => {
                                    return Err(not_pushed(
                                        Refusal::Sort,
                                        "a sort over a join, or after a selection, a fold or \
                                         another sort (a plan has one order)"
                                            .into(),
                                    ));
                                }
                                (None, _) => {
                                    return Err(mismatch(
                                        Refusal::Sort,
                                        "ORDER BY collations differ per engine".into(),
                                    ));
                                }
                            };
                            let mut desc = false;
                            for &k in &kids[1..] {
                                let named =
                                    (self.kind(k) == "option").then(|| self.prop_s(k, "name"));
                                match named.as_deref() {
                                    Some("--desc" | "-d") => desc = true,
                                    _ => {
                                        return Err(mismatch(
                                            Refusal::Sort,
                                            "ORDER BY collations differ per engine".into(),
                                        ));
                                    }
                                }
                            }
                            let e = self.operand(kids[0], None)?;
                            self.assume_numeric(Some(column.clone()));
                            self.order.sort = Some((e, desc));
                            self.order.keyed_by = Some((Refusal::Sort, column));
                        }
                        "sort_by" => {
                            // A sort after a positional selection
                            // (or a second sort) cannot render: the
                            // fixed SELECT shape orders before
                            // LIMIT, and has one ORDER BY.
                            if sel.limit.is_some() {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "a sort after a positional selection (SQL orders \
                                     before LIMIT)"
                                        .into(),
                                ));
                            }
                            if sel.order_by.is_some() {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "a second sort (SQL has a single ORDER BY)".into(),
                                ));
                            }
                            let kids = self.arbor.children(s);
                            // The options are the command's own:
                            // `--desc` is the direction, a named
                            // collation (`--locale=tag`, ruling
                            // 2026-11-11) is the engine's order and no
                            // backend's; a second key has no place in
                            // the one ORDER BY this shape holds.
                            let mut desc = false;
                            for &k in &kids[1..] {
                                if self.kind(k) != "option" {
                                    return Err(not_pushed(
                                        Refusal::Sort,
                                        "a sort by several keys".into(),
                                    ));
                                }
                                match self.prop_s(k, "name").as_str() {
                                    "--desc" | "-d" => desc = true,
                                    "--locale" => {
                                        return Err(mismatch(
                                            Refusal::Sort,
                                            "sort_by(--locale=…) names the engine's collation; \
                                             a backend's differs"
                                                .into(),
                                        ));
                                    }
                                    // a case order is that
                                    // collation's (ruling 2026-11-12)
                                    "--upper-first" | "-u" | "--lower-first" | "-l"
                                    | "--ignore-case" | "-i" => {
                                        return Err(mismatch(
                                            Refusal::Sort,
                                            "sort_by's case order is the engine's collation's; \
                                             a backend's differs"
                                                .into(),
                                        ));
                                    }
                                    other => {
                                        return Err(not_pushed(
                                            Refusal::Sort,
                                            format!("the '{other}' option of sort_by"),
                                        ));
                                    }
                                }
                            }
                            let e = self.operand(kids[0], join.map(|(l, _)| l))?;
                            sel.order_by = Some((e, desc));
                        }
                        // The context reversed: every term of the
                        // plan's order turned, the keys too.
                        "reverse" if self.strict => {
                            if self.order.reversed
                                || self.order.limit.is_some()
                                || self.order.groups
                                || self.aggregate
                                || grouped.is_some()
                            {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "reverse after a positional selection, a fold or another \
                                     reverse"
                                        .into(),
                                ));
                            }
                            self.order.reversed = true;
                        }
                        "reverse" => match &mut sel.order_by {
                            Some((_, desc)) => *desc = true,
                            None => {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "reverse without an ORDER BY (rows are unordered)".into(),
                                ));
                            }
                        },
                        // `top(k; ::n)` and `bottom(k; ::n)` in a plan:
                        // the rows whose key is not null, by the key
                        // descending or ascending, the ties in the
                        // table's order, and the first k of them. A
                        // sort and a cut, so the sort's conditions:
                        // one numeric column of the table, nothing
                        // ordered or cut before.
                        "top" | "bottom" if self.strict => {
                            let kids = self.arbor.children(s);
                            let count = kids.first().and_then(|&k| {
                                (self.kind(k) == "literal")
                                    .then(|| self.prop(k, "value"))
                                    .flatten()
                            });
                            let column = kids.get(1).and_then(|&k| self.column_operand(k));
                            let plain = kids.len() == 2
                                && join.is_none()
                                && self.order.sort.is_none()
                                && self.order.limit.is_none()
                                && !self.order.reversed
                                && !self.order.groups
                                && !self.aggregate
                                && grouped.is_none();
                            let (Some(Value::Int(k)), Some(column), true) = (count, column, plain)
                            else {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "'top' and 'bottom' by anything but one column of the \
                                     table, under an option, over a join, or after a sort, a \
                                     selection or a fold (a plan has one order)"
                                        .into(),
                                ));
                            };
                            if k < 1 {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "'top' and 'bottom' of no rows".into(),
                                ));
                            }
                            let e = self.operand(kids[1], None)?;
                            sel.wheres.push(format!("{e} IS NOT NULL"));
                            self.assume_numeric(Some(column.clone()));
                            self.order.sort = Some((e, name == "top"));
                            self.order.limit = Some(k as u64);
                            self.order.keyed_by = Some((Refusal::Sort, column));
                        }
                        "top" => {
                            if sel.limit.is_some() {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "'top' after a positional selection (SQL orders \
                                     before LIMIT)"
                                        .into(),
                                ));
                            }
                            if sel.order_by.is_some() {
                                return Err(not_pushed(
                                    Refusal::Sort,
                                    "a second sort (SQL has a single ORDER BY)".into(),
                                ));
                            }
                            let kids = self.arbor.children(s);
                            // the options name the engine's collation
                            // and its case order: no backend's (they
                            // were dropped, and the SQL ordered as the
                            // backend pleased)
                            if let Some(&k) = kids
                                .get(2..)
                                .and_then(|rest| rest.iter().find(|&&k| self.kind(k) == "option"))
                            {
                                return Err(mismatch(
                                    Refusal::Sort,
                                    format!(
                                        "top({}) names the engine's collation; a backend's differs",
                                        self.prop_s(k, "name")
                                    ),
                                ));
                            }
                            let n = self.prop_s(kids[0], "value");
                            let e = self.operand(kids[1], join.map(|(l, _)| l))?;
                            sel.order_by = Some((e, true));
                            sel.limit = Some(n);
                        }
                        "unique" => {
                            if self.strict {
                                // A count of the distinct values asks
                                // for no order: `| ::n @| unique @|
                                // count`, the last two commands of the
                                // pipeline. COUNT(DISTINCT n) leaves
                                // the NULLs out, and a null is one
                                // value to `unique`: the plan adds it.
                                // The column is a numeric one: two
                                // texts that read as one number
                                // ('300', '300.0') are one value to
                                // the engine and two to DISTINCT.
                                let counted = self.arbor.children(s).is_empty()
                                    && i + 2 == commands.len()
                                    && commands.get(i + 1).is_some_and(|&c| {
                                        self.kind(c) == "agg"
                                            && self.prop_s(c, "name") == "count"
                                            && self.prop(c, "group").is_none()
                                            && self.arbor.children(c).is_empty()
                                    });
                                match (counted, pending_col.take(), pending_name.take()) {
                                    (true, Some(col), Some(name)) => {
                                        self.assume_numeric(Some(name.clone()));
                                        self.order.keyed_by = Some((Refusal::Unique, name));
                                        sel.select = vec![format!(
                                            "COUNT(DISTINCT {col}) + COALESCE(MAX(CASE WHEN \
                                             {col} IS NULL THEN 1 ELSE 0 END), 0)"
                                        )];
                                        self.aggregate = true;
                                        // the count is taken with it
                                        i += 2;
                                        continue;
                                    }
                                    _ => {
                                        return Err(not_pushed(
                                            Refusal::Unique,
                                            "DISTINCT returns rows in no stated order (a count \
                                             of the distinct values of a numeric column is \
                                             pushed: '| ::col @| unique @| count')"
                                                .into(),
                                        ));
                                    }
                                }
                            }
                            // Quarb dedups the limited rows; SQL
                            // applies DISTINCT before LIMIT.
                            if sel.limit.is_some() {
                                return Err(not_pushed(
                                    Refusal::Unique,
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
                            return Err(not_pushed(
                                Refusal::Reducer,
                                format!("the '{other}' aggregate"),
                            ));
                        }
                    }
                }
                // `@| [..n]` in a plan: the first rows of the order the
                // driver writes (the plan's own sort, then the keys),
                // so the rows the scan's context begins with.
                "select" if self.strict => {
                    if self.order.limit.is_some()
                        || self.order.groups
                        || self.aggregate
                        || grouped.is_some()
                        || pushed_key.is_some()
                    {
                        return Err(not_pushed(
                            Refusal::Take,
                            "a positional selection after a reducer, a fold or another \
                             selection"
                                .into(),
                        ));
                    }
                    let p = self.arbor.children(s)[0];
                    match (self.prop_s(p, "kind").as_str(), self.prop(p, "to")) {
                        ("range", Some(Value::Int(n)))
                            if self.prop(p, "from").is_none() && n > 0 =>
                        {
                            self.order.limit = Some(n as u64);
                        }
                        _ => {
                            return Err(not_pushed(
                                Refusal::Take,
                                "positional selection beyond '@| [..n]'".into(),
                            ));
                        }
                    }
                }
                "select" => {
                    // `@| [..n]` → LIMIT.
                    if sel.limit.is_some() {
                        return Err(not_pushed(
                            Refusal::Take,
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
                            return Err(not_pushed(
                                Refusal::Take,
                                "positional selection beyond '@| [..n]'".into(),
                            ));
                        }
                    }
                }
                other => {
                    return Err(not_pushed(
                        Refusal::Operator,
                        format!(
                            "the '{other}' command (windows, subcontexts, and registers are \
                         Quarb-side state)"
                        ),
                    ));
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
        // A plan answers row nodes as their rows, where the scan
        // answers the nodes: a sorted, reversed or cut selection of
        // nodes was the scan's to answer, and stays so; with a
        // projection or a record after it, it is the plan's.
        if self.strict
            && sel.select.is_empty()
            && (self.order.sort.is_some() || self.order.reversed || self.order.limit.is_some())
        {
            let word = if self.order.limit.is_some() {
                Refusal::Take
            } else {
                Refusal::Sort
            };
            return Err(not_pushed(
                word,
                "a sorted, reversed or cut selection of row nodes (project a column or build \
                 a record after it: '| ::col', '| %(…)')"
                    .into(),
            ));
        }
        // A grouped reduction never recorded (`@|{.k} count` with no
        // `| .n | %.`) answers one value per group: the aggregate
        // alone, grouped by the key.
        if let Some((_, agg, _)) = grouped
            && sel.select.is_empty()
        {
            sel.select = vec![agg];
        }
        Ok(())
    }

    /// The column a pushed projection body names — `.k(::k)` is a
    /// subcontext whose one branch is the bare projection; any
    /// other body is Quarb-side state.
    fn pushed_column(&mut self, s: NodeId, qual: Option<&str>) -> Result<String, SqlError> {
        let body = || {
            let q = self.kid(s, "query")?;
            let b = self.kid(q, "branch")?;
            let p = self.kid(b, "projection")?;
            let bare = self.kids(q, "branch").len() == 1
                && self.kids(q, "pipeline").is_empty()
                && self.arbor.children(b).len() == 1
                && self.prop_s(p, "kind") == "property";
            bare.then_some(p)
        };
        let p = body()
            .ok_or_else(|| not_pushed(Refusal::State, "a pushed body (Quarb-side state)".into()))?;
        let col = self.projection_col(p)?;
        let col = self.sql_ident(&col, "column name")?;
        Ok(match qual {
            Some(q) => format!("{q}.{col}"),
            None => col,
        })
    }

    /// A HAVING filter: `$_` and `$.name` refer to the aggregate.
    fn having_cond(&mut self, s: NodeId) -> Result<String, SqlError> {
        let kids = self.arbor.children(s);
        if kids.len() != 1 || self.kind(kids[0]) != "compare" {
            return Err(not_pushed(
                Refusal::Fold,
                "HAVING translates for a single comparison".into(),
            ));
        }
        let e = kids[0];
        let op = self.prop_s(e, "op");
        let cmp_kids = self.arbor.children(e);
        let l = match self.kind(cmp_kids[0]).as_str() {
            "topic" | "recall" => "__AGG__".to_string(),
            _ => {
                return Err(not_pushed(
                    Refusal::Fold,
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

impl Exporter {
    /// What a reducer's own arguments ask of the plan. A pushed
    /// reducer is the bare command: `count` is COUNT(*), `sum` is
    /// SUM(col). `count(--values)` counts the values other than
    /// null, which over a projected column is what COUNT(col)
    /// counts.
    /// Any other option, and any operand, changes what is reduced
    /// (`--nulls`, `--of=`, `--by=`), and the reducer stays with the
    /// engine. The plan took no notice of them until 0.68:
    /// `count(--values)` and `count(--nulls)` were both answered as
    /// COUNT(*), where the scan counts five and one of six.
    fn reducer_counts_values(
        &self,
        s: NodeId,
        name: &str,
        projected: bool,
    ) -> Result<bool, SqlError> {
        let mut values = false;
        for k in self.arbor.children(s) {
            if self.kind(k) != "option" {
                return Err(not_pushed(
                    Refusal::Reducer,
                    format!("an operand of '{name}'"),
                ));
            }
            match (name, self.prop_s(k, "name").as_str()) {
                ("count", "--values" | "-v") => values = true,
                (_, option) => {
                    return Err(not_pushed(
                        Refusal::Reducer,
                        format!("the '{option}' option of '{name}'"),
                    ));
                }
            }
        }
        // a row node is no value: what the count answers there is
        // the engine's to say
        if values && !projected {
            return Err(not_pushed(
                Refusal::Reducer,
                "'count(--values)' over row nodes (project a column: '/table/*::col @| \
                 count(--values)')"
                    .into(),
            ));
        }
        Ok(values)
    }
}

fn sql_agg(name: &str, col: Option<String>) -> Result<String, SqlError> {
    let f = match name {
        "count" => "COUNT",
        "sum" => "SUM",
        "mean" => "AVG",
        "min" => "MIN",
        "max" => "MAX",
        _ => unreachable!("checked by caller"),
    };
    match col {
        Some(c) => Ok(format!("{f}({c})")),
        // Only COUNT aggregates bare rows; SUM(*) and friends are
        // not SQL.
        None if f == "COUNT" => Ok("COUNT(*)".to_string()),
        None => Err(not_pushed(
            Refusal::Reducer,
            format!("'{name}' over row nodes (project a column: '/table/*::col @| {name}')"),
        )),
    }
}

#[cfg(test)]
mod null_and_literal_tests {
    use super::{Dialect, export, partial_pushdown, pushdown};

    // Grouped pipeline that never pushes, so `partial_pushdown` hinges
    // only on the leading predicate (mirrors the crate's partial gate).
    const GROUPED: &str = " | ::x | .g(::x) @|{.g} count | .n | %.";

    #[test]
    fn pattern_literal_pushes_as_like() {
        // Ruling #33: segments in order, a `%` per star, LIKE's
        // metacharacters escaped. Strict push only where LIKE is
        // provably case-sensitive (PostgreSQL); everywhere else the
        // pattern rides the scan.
        let q = "/users/*[::name == 'app-'*]::id";
        let p = pushdown(q, Some(Dialect::Postgres)).unwrap();
        assert_eq!(
            p.sql,
            "SELECT id FROM users WHERE name LIKE 'app-%' ESCAPE '\\'"
        );
        assert!(pushdown(q, Some(Dialect::Sqlite)).is_none());
        assert!(pushdown(q, Some(Dialect::MySql)).is_none());
        // contains and multi-segment shapes
        let p = pushdown("/users/*[::name == *'web'*]::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%web%'"), "{}", p.sql);
        let p = pushdown(
            "/users/*[::name == *'a'*'.gz']::id",
            Some(Dialect::Postgres),
        )
        .unwrap();
        assert!(p.sql.contains("LIKE '%a%.gz'"), "{}", p.sql);
        // LIKE metacharacters in a segment are escaped
        let p = pushdown("/users/*[::name == *'50%_x'*]::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%50\\%\\_x%'"), "{}", p.sql);
        // `!=` with a pattern rides the scan
        assert!(pushdown("/users/*[::name !== *'web'*]::id", Some(Dialect::Postgres)).is_none());
        // the `*=` alias gains the same PostgreSQL rung
        let p = pushdown("/users/*[::name *= 'web']::id", Some(Dialect::Postgres)).unwrap();
        assert!(p.sql.contains("LIKE '%web%'"), "{}", p.sql);
        // and on SQLite it is instr, a substring test by bytes and by
        // case as `*=` is; MySQL's INSTR folds case, and scans
        assert_eq!(
            pushdown("/users/*[::name *= 'web']::id", Some(Dialect::Sqlite))
                .unwrap()
                .sql,
            "SELECT id FROM users WHERE instr(name, 'web') > 0"
        );
        assert!(pushdown("/users/*[::name *= 'web']::id", Some(Dialect::MySql)).is_none());
        assert!(pushdown("/users/*[::name *= 'web']::id", None).is_none());
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
        // Numeric comparison, `!=`, and a wildcard step stay off the
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
    fn a_regex_names_the_literals_it_cannot_match_without() {
        let lits = |p: &str| super::required_literals(p);
        assert_eq!(lits("^Ma.*s$"), ["Ma", "s"]);
        assert_eq!(lits("(us)+$"), ["us"]);
        assert_eq!(lits("a?b"), ["b"]);
        assert_eq!(lits("(?:foo|bar)baz"), ["baz"]);
        assert_eq!(lits("fo{2,}"), ["f", "o"]);
        assert_eq!(lits(r"\d+-\d+"), ["-"]);
        assert_eq!(lits(r"\bfoo\b"), ["foo"]);
        assert_eq!(lits(r"No\.1"), ["No.1"]);
        assert_eq!(lits("na\u{ef}ve"), ["na\u{ef}ve"]);
        assert_eq!(lits("(?x) a b"), ["ab"]);
        assert_eq!(lits("(a|b)c*d"), ["d"]);
        assert_eq!(lits("x{0}y"), ["y"]);
        assert_eq!(lits("ab(ab)"), ["ab"]);
        // nothing promised: a class, an alternation, an optional
        // part, a letter under (?i), a pattern that does not parse
        for p in ["(?i)ma", "Ven|Jup", "a*", "[ab]", "(ab)?", ".*", "", "["] {
            assert!(lits(p).is_empty(), "{p}");
        }
    }

    #[test]
    fn a_regex_is_narrowed_by_its_literals() {
        let part = |q: &str, d: Dialect| {
            partial_pushdown(q, Some(d)).map(|p| (p.where_sql, p.column_types.text))
        };
        assert_eq!(
            part("/t/*[::a == (/^Ma.*s$/)]::b", Dialect::Sqlite),
            Some((
                "instr(a, 'Ma') > 0 AND instr(a, 's') > 0".to_string(),
                vec!["a".to_string()]
            ))
        );
        assert_eq!(
            part("/t/*[::a == (/50%_off/)]::b", Dialect::Postgres).map(|p| p.0),
            Some(r"a LIKE '%50\%\_off%' ESCAPE '\'".to_string())
        );
        // a conjunction keeps the side that translates; a
        // disjunction needs both
        assert_eq!(
            part("/t/*[::n > 2 && ::a == (/on/)]::b", Dialect::Sqlite).map(|p| p.0),
            Some("(n > 2 AND instr(a, 'on') > 0)".to_string())
        );
        assert_eq!(
            part(
                "/t/*[::a == (/(?i)x/) && ::a == (/on/)]::b",
                Dialect::Sqlite
            )
            .map(|p| p.0),
            Some("(instr(a, 'on') > 0)".to_string())
        );
        assert_eq!(
            part("/t/*[::a == (/Ven/) || ::a == (/Jup/)]::b", Dialect::Sqlite).map(|p| p.0),
            Some("(instr(a, 'Ven') > 0 OR instr(a, 'Jup') > 0)".to_string())
        );
        for q in [
            // one side promises nothing: the disjunction is not narrowed
            "/t/*[::a == (/Ven/) || ::a == (/(?i)jup/)]::b",
            // a negation is never narrowed, nor a non-match
            "/t/*[!(::a == (/Ven/))]::b",
            "/t/*[::a !== (/Ven/)]::b",
            // a literal a SQL string would not carry everywhere
            "/t/*[::a == (/it's/)]::b",
            // no literal at all
            "/t/*[::a == (/(?i)ven/)]::b",
            "/t/*[::a == (/Ven|Jup/)]::b",
        ] {
            assert_eq!(part(q, Dialect::Sqlite), None, "{q}");
        }
        // MySQL's instr follows the collation: it keeps every text
        // that holds the literal, and some in another case (0.71)
        assert_eq!(
            part("/t/*[::a == (/Ven/)]::b", Dialect::MySql).map(|p| p.0),
            Some("instr(a, 'Ven') > 0".to_string())
        );
        // under no dialect (DuckDB's, BigQuery's and Athena's doors)
        assert_eq!(
            partial_pushdown("/t/*[::a == (/^Ma.*s$/)]::b", None).map(|p| p.where_sql),
            Some("strpos(a, 'Ma') > 0 AND strpos(a, 's') > 0".to_string())
        );
        assert_eq!(
            partial_pushdown("/t/*[::n > 2 && ::a == (/on/)]::b", None).map(|p| p.where_sql),
            Some("(n > 2 AND strpos(a, 'on') > 0)".to_string())
        );
        // SQL Server and Oracle: not narrowed
        assert_eq!(part("/t/*[::a == (/Ven/)]::b", Dialect::Mssql), None);
        assert_eq!(part("/t/*[::a == (/Ven/)]::b", Dialect::Oracle), None);
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
        let p = partial_pushdown("/orders/*[/payload/gift]::id", Some(Dialect::Postgres)).unwrap();
        assert_eq!(
            p.where_sql,
            "(jsonb_typeof(payload::jsonb #> '{gift}') IS NOT NULL)"
        );
    }

    #[test]
    fn json_property_path_pushdown() {
        // `[/col/a::b = 'lit']`, the property form: the scalar's
        // question and, on an array of scalars, the list's (any item
        // equals). SQLite's condition says both whole.
        let q = "/orders/*[/payload/customer/geo::city = 'Lyon']::id";
        assert_eq!(
            pushdown(q, Some(Dialect::Sqlite)).unwrap().sql,
            "SELECT id FROM orders WHERE (json_valid(payload) AND \
             ((json_type(payload, '$.customer.geo.city') = 'text' AND \
             json_extract(payload, '$.customer.geo.city') = 'Lyon') OR \
             (json_type(payload, '$.customer.geo.city') = 'array' AND \
             EXISTS (SELECT 1 FROM json_each(payload, '$.customer.geo.city') \
             WHERE json_each.type = 'text' AND json_each.value = 'Lyon') AND \
             NOT EXISTS (SELECT 1 FROM json_each(payload, '$.customer.geo.city') \
             WHERE json_each.type IN ('array', 'object')))))"
        );
        // a property of the column's own value
        assert!(
            pushdown(
                "/orders/*[/payload::city = 'Lyon']::id",
                Some(Dialect::Sqlite)
            )
            .unwrap()
            .sql
            .contains("json_extract(payload, '$.city') = 'Lyon'")
        );
        // the other engines' array branches are not verified: no
        // whole push there, and none with no dialect
        for d in [
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::Mssql,
            Dialect::Oracle,
        ] {
            assert!(pushdown(q, Some(d)).is_none(), "{d:?}");
        }
        assert!(pushdown(q, None).is_none());
        // a numeric-looking literal compares by value: no whole push
        assert!(
            pushdown(
                "/orders/*[/payload::total = '150']::id",
                Some(Dialect::Sqlite)
            )
            .is_none()
        );
        // a column named as one of json_each's own columns would be
        // read as that column inside the subquery
        assert!(
            pushdown(
                "/orders/*[/value::city = 'Lyon']::id",
                Some(Dialect::Sqlite)
            )
            .is_none()
        );
    }

    #[test]
    fn partial_json_property_prefilters() {
        let sqlite = |q: &str| partial_pushdown(q, Some(Dialect::Sqlite)).map(|p| p.where_sql);
        // a comparison lets the arrays through with the text values:
        // a list matches when any item does, and the engine decides
        assert_eq!(
            sqlite("/orders/*[/payload::total > 150]::id").unwrap(),
            "(json_valid(payload) AND (json_extract(payload, '$.total') > 150 \
             OR json_type(payload, '$.total') IN ('text', 'array')))"
        );
        assert_eq!(
            sqlite("/orders/*[/payload/customer::tier < 'm']::id").unwrap(),
            "(json_valid(payload) AND (json_extract(payload, '$.customer.tier') < 'm' \
             OR json_type(payload, '$.customer.tier') = 'array'))"
        );
        // a bare property: present, as the child form
        assert_eq!(
            sqlite("/orders/*[/payload::gift]::id").unwrap(),
            "(json_valid(payload) AND json_type(payload, '$.gift') IS NOT NULL)"
        );
        // `!=` keeps a row whose key is absent: its floor is the node
        // the property is read on, and on the column itself there is
        // none
        assert_eq!(
            sqlite("/orders/*[/payload/customer::tier != 'x']::id").unwrap(),
            "(json_valid(payload) AND json_type(payload, '$.customer') IS NOT NULL)"
        );
        assert!(sqlite("/orders/*[/payload::status != 'x']::id").is_none());
        // PostgreSQL: the equality as a prefilter, the arrays through
        assert_eq!(
            partial_pushdown(
                "/orders/*[/payload/customer/geo::city = 'Lyon']::id",
                Some(Dialect::Postgres)
            )
            .unwrap()
            .where_sql,
            "(CASE WHEN jsonb_typeof(payload::jsonb #> '{customer,geo,city}') = 'string' \
             THEN (payload::jsonb #>> '{customer,geo,city}') = 'Lyon' \
             WHEN jsonb_typeof(payload::jsonb #> '{customer,geo,city}') = 'array' \
             THEN true ELSE false END)"
        );
        assert_eq!(
            partial_pushdown(
                "/orders/*[/payload::total > 150]::id",
                Some(Dialect::Postgres)
            )
            .unwrap()
            .where_sql,
            "(CASE WHEN jsonb_typeof(payload::jsonb #> '{total}') = 'number' \
             THEN ((payload::jsonb #>> '{total}'))::float8 > 150 \
             WHEN jsonb_typeof(payload::jsonb #> '{total}') IN ('string', 'array') \
             THEN true ELSE false END)"
        );
        // no type probe, no prefilter
        assert!(
            partial_pushdown("/orders/*[/payload::total > 150]::id", Some(Dialect::MySql))
                .is_none()
        );
    }

    #[test]
    fn json_pushdown_refuses_a_container_looking_literal() {
        // An extractor answers an array or an object as its JSON
        // text, so `'[1,2]'` equalled the array itself, which has no
        // value in the graft: the whole push kept a row the scan
        // drops. It rides the prefilter (a superset, re-checked).
        for q in [
            "/orders/*[/data/a:: = '[1,2]']::id",
            "/orders/*[/data/a:: = '{\"k\":1}']::id",
        ] {
            assert!(pushdown(q, Some(Dialect::Sqlite)).is_none(), "{q}");
            assert!(pushdown(q, Some(Dialect::Postgres)).is_none(), "{q}");
        }
        assert_eq!(
            partial_pushdown("/orders/*[/data/a:: = '[1,2]']::id", Some(Dialect::Sqlite))
                .unwrap()
                .where_sql,
            "(json_valid(data) AND json_extract(data, '$.a') = '[1,2]')"
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
    fn ne_and_not_push_in_the_form_that_keeps_the_null_rows() {
        // `!=` against a value and `!(...)` hold for a NULL in Quarb
        // and are UNKNOWN in SQL. The plan says the cases SQL's
        // operators leave out, in SQL every engine reads (the scan's
        // answers beside the plan's: tests/sql.rs).
        assert_eq!(
            pushdown("/t/*[::x != 5] | ::x", None).unwrap().sql,
            "SELECT x FROM t WHERE (x <> 5 OR x IS NULL)"
        );
        assert_eq!(
            pushdown("/t/*[::x != ::y] | ::x", None).unwrap().sql,
            "SELECT x FROM t WHERE (x <> y OR (x IS NULL AND y IS NOT NULL) OR (x IS NOT NULL \
             AND y IS NULL))"
        );
        assert_eq!(
            pushdown("/t/*[!::x = 5] | ::x", None).unwrap().sql,
            "SELECT x FROM t WHERE (CASE WHEN x = 5 THEN 0 ELSE 1 END = 1)"
        );
        // two NULLs are equal in Quarb: an equality of two columns
        // says so
        assert_eq!(
            pushdown("/t/*[::x = ::y] | ::x", None).unwrap().sql,
            "SELECT x FROM t WHERE (x = y OR (x IS NULL AND y IS NULL))"
        );
        // the prefilter takes them too
        assert_eq!(
            partial_pushdown(&format!("/t/*[::x != 5]{GROUPED}"), None)
                .unwrap()
                .where_sql,
            "(x <> 5 OR x IS NULL)"
        );
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
            pushdown("/t/*[::name = \"rare\"] | ::name", None)
                .unwrap()
                .sql,
            "SELECT name FROM t WHERE name = 'rare'"
        );
    }
}
