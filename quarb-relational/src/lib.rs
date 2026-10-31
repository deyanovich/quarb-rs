//! The shared relational model behind Quarb's database adapters.
//!
//! A relational database maps onto the arbor as a two-level tree
//! with the schema's foreign keys as its crosslink fabric: tables
//! as the unnamed root's children, rows named by primary key,
//! columns as properties, and declared foreign keys driving `-->`
//! resolution (with chains), `->`/`<-` crosslinks labeled by
//! column, and `<--` reverse resolution. The reference label names a
//! target table for undeclared references.
//!
//! This crate is engine-independent: an engine adapter
//! (`quarb-sqlite`, `quarb-postgres`, …) is a thin *catalog
//! driver* — it connects, introspects its catalog into
//! [`TableSpec`]s, and supplies rows.
//!
//! **Loading model: catalog eager, rows lazy, hops by key.** The
//! schema is read once at open. A table is read *whole* only when a
//! query asks for all of it — navigating into its rows (`/t/*`), its
//! `:::n-rows`, a walk — via the driver-supplied [`Fetcher`]. A hop
//! reads the rows it lands on: a `-->` or `->` target, `/t/KEY`, the
//! referrers of `<-` and `<--`, through the driver's [`KeyFetcher`]
//! (`WHERE key IN (…)`, one statement per frontier where the engine
//! batches). Rows are cached for the adapter's lifetime and keep
//! their identity when the table is later read whole. A driver
//! without a keyed source, a table without a stable row key, and a
//! table that has answered [`KEYED_BUDGET`] keyed statements read
//! whole, as before. Untouched tables are never read. A fetch failure
//! surfaces as an empty table plus a warning on stderr (the adapter
//! trait has no error channel mid-navigation); `:::loaded` on a
//! table node reports whether it has materialized.
//! [`RelationalModel::build`] remains the fully-eager form (used by
//! in-memory loads and tests).

use quarb::{AstAdapter, NodeId, Value};
use std::collections::HashMap;

/// Parse a declared-references document — the client-side rung of
/// the reference-provenance ladder, for substrates whose catalog
/// cannot hold the mapping (a schemaless store, or a schema whose
/// containers are views): `{"refs": {"field": "container", ...}}`.
/// A field is a bare column name, or `table.column` to scope the
/// declaration to one table. Returns the (field, container) pairs
/// in document order.
/// The statement a driver last executed, verbatim — recorded at
/// the moment of execution because the ORDER BY key is a
/// catalog lookup each driver makes for itself, so the final SQL
/// exists nowhere earlier. `qua --explain` prints what was run,
/// not a description of what will be.
mod executed {
    use std::cell::RefCell;
    thread_local! {
        pub(super) static SQL: RefCell<Option<String>> = const { RefCell::new(None) };
    }
}

/// Record the statement about to be executed (drivers call this).
pub fn record_executed(sql: &str) {
    executed::SQL.with(|c| *c.borrow_mut() = Some(sql.to_string()));
}

/// Take the last recorded statement, if any (the tool layer).
pub fn take_executed() -> Option<String> {
    executed::SQL.with(|c| c.borrow_mut().take())
}

pub fn parse_refs(text: &str) -> Result<Vec<(String, String)>, String> {
    let doc: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let map = doc
        .get("refs")
        .and_then(|r| r.as_object())
        .ok_or_else(|| "expected {\"refs\": {\"field\": \"container\", ...}}".to_string())?;
    let mut out = Vec::new();
    for (field, target) in map {
        let container = target
            .as_str()
            .ok_or_else(|| format!("ref target for '{field}' must be a string"))?;
        out.push((field.clone(), container.to_string()));
    }
    Ok(out)
}

/// One table's shape, as introspected by an engine adapter.
pub struct TableSpec {
    pub name: String,
    pub columns: Vec<String>,
    /// The single-column primary key's index, if any — it names the
    /// rows. `None` (absent or composite) falls back to the row id.
    pub pk: Option<usize>,
    /// Declared foreign keys: (column index, target table, target
    /// column; an empty target column means the target's key).
    pub fks: Vec<(usize, String, String)>,
    /// The columns the schema types json or jsonb: their cells
    /// declare the JSON notation and graft as JSON (spec: Grafting).
    pub json_columns: Vec<usize>,
    /// Each column's declared type as the catalog spells it
    /// (`VARCHAR(20)`, `integer`, `timestamp with time zone`), one
    /// per column, empty where the source declares none; the cells
    /// carry it as a trait (spec: adapter traits, [`type_traits`]).
    pub column_types: Vec<String>,
}

impl TableSpec {
    /// The label of each declared key's crosslink, aligned with
    /// `fks`: the column without its `_id` suffix, the later of two
    /// that would share a name keeping its full column name
    /// ([`quarb::adapter::reference_labels`]).
    pub fn fk_labels(&self) -> Vec<String> {
        // declaration order is the columns' order, whatever order
        // the catalog lists the keys in
        let mut order: Vec<usize> = (0..self.fks.len()).collect();
        order.sort_by_key(|&i| self.fks[i].0);
        let labels = quarb::adapter::reference_labels(
            order.iter().map(|&i| self.columns[self.fks[i].0].as_str()),
        );
        let mut out = vec![String::new(); self.fks.len()];
        for (label, i) in labels.into_iter().zip(order) {
            out[i] = label;
        }
        out
    }
}

/// The traits a declared SQL type gives a cell (spec: adapter
/// traits): the type name lowercased and stripped of its parameters
/// (`VARCHAR(20)` → `varchar`, `DECIMAL(18,3)` → `decimal`,
/// `INTEGER[]` → `integer`), and beside it the derived kind —
/// `number` for integer and floating-point types, `string` for the
/// character types, `boolean` for the boolean ones — so one spelling
/// selects the kind whatever the source. An empty declaration gives
/// no trait.
pub fn type_traits(declared: &str) -> Vec<String> {
    let raw = declared.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return Vec::new();
    }
    let name = raw
        .split(['(', '['])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return Vec::new();
    }
    let kind = if raw.starts_with("tinyint(1)") || raw == "bit(1)" {
        // MySQL's BOOLEAN is tinyint(1); SQL Server's is bit.
        Some("boolean")
    } else {
        match name.as_str() {
            "boolean" | "bool" | "bit" => Some("boolean"),
            "int" | "integer" | "smallint" | "bigint" | "tinyint" | "mediumint" | "int2"
            | "int4" | "int8" | "serial" | "bigserial" | "smallserial" | "real" | "float"
            | "float4" | "float8" | "double" | "double precision" | "numeric" | "decimal"
            | "number" | "dec" | "hugeint" | "ubigint" | "uinteger" | "usmallint" | "utinyint"
            | "float64" | "int64" | "numeric64" | "bignumeric" | "binary_float"
            | "binary_double" => Some("number"),
            "char" | "character" | "varchar" | "character varying" | "nvarchar" | "nchar"
            | "varchar2" | "nvarchar2" | "text" | "ntext" | "clob" | "nclob" | "tinytext"
            | "mediumtext" | "longtext" | "string" | "bpchar" | "citext" | "name" => Some("string"),
            _ => None,
        }
        .or_else(|| {
            // SQLite's declared types are free text under its
            // affinity rules (`UNSIGNED BIG INT`, `VARYING CHARACTER`).
            if name.contains("int") {
                Some("number")
            } else if name.contains("char") || name.contains("clob") || name.contains("text") {
                Some("string")
            } else if name.contains("bool") {
                Some("boolean")
            } else if name.contains("real") || name.contains("floa") || name.contains("doub") {
                Some("number")
            } else {
                None
            }
        })
    };
    let mut out = vec![name];
    if let Some(k) = kind {
        out.push(k.to_string());
    }
    out
}

/// One row: an engine-side row identifier (SQLite's `rowid`, or an
/// ordinal) and the column values. `None` is a row read by key
/// whose place in the table is not yet known (any integer,
/// including 0, is a legal SQLite rowid).
pub struct RowSpec {
    pub rowid: Option<i64>,
    pub values: Vec<Value>,
}

/// The driver-supplied row source: given a table's index and spec,
/// stream its rows. Called at most once per table.
pub type Fetcher = Box<dyn Fn(usize, &TableSpec) -> Result<Vec<RowSpec>, String>>;

/// The driver-supplied keyed row source: the rows of a table whose
/// column — `Some(index)`, or `None` for the row key (the primary
/// key, else the driver's row id) — holds one of `values`, in the
/// table's own order (`SELECT … WHERE col IN (…) ORDER BY <key>`).
/// A hop then reads the rows it lands on instead of the table. An
/// `Err` (a table whose rows have no stable key, a statement the
/// server rejects) sends the model to the whole-table fetch for
/// that table, so a driver may decline case by case.
pub type KeyFetcher =
    Box<dyn Fn(usize, &TableSpec, Option<usize>, &[Value]) -> Result<Vec<RowSpec>, String>>;

/// A driver's row source with an optional WHERE: the one function
/// from which [`RelationalModel::lazy_where`] derives the whole
/// read, the filtered listing and the keyed read.
pub type WhereFetcher =
    std::rc::Rc<dyn Fn(usize, &TableSpec, Option<&str>) -> Result<Vec<RowSpec>, String>>;

/// The `WHERE` text for a list of column equalities, each column
/// quoted by `quote`, each value as a portable SQL literal
/// ([`sql_literal`]); `None` for an empty list or a value with no
/// literal. The text every SQL driver's estimator puts after
/// `WHERE` for the plan's conjuncts.
pub fn where_equals(
    spec: &TableSpec,
    equals: &[(usize, Value)],
    quote: fn(&str) -> String,
) -> Option<String> {
    if equals.is_empty() {
        return None;
    }
    let parts: Option<Vec<String>> = equals
        .iter()
        .map(|(c, v)| {
            Some(format!(
                "{} = {}",
                quote(spec.columns.get(*c)?),
                sql_literal(v)?
            ))
        })
        .collect();
    Some(parts?.join(" AND "))
}

/// ` WHERE …` for the equalities, or nothing for none: what a
/// driver appends to the statement it prices.
pub fn where_suffix(
    spec: &TableSpec,
    equals: &[(usize, Value)],
    quote: fn(&str) -> String,
) -> String {
    match where_equals(spec, equals, quote) {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    }
}

/// The first `key=NUMBER` in a planner's text (`rows=1234`,
/// `width=8`), for the dialects whose `EXPLAIN` is prose.
pub fn plan_number(text: &str, key: &str) -> Option<u64> {
    let i = text.find(key)? + key.len();
    let digits: String = text[i..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// An estimate from a planner's row count and average row width —
/// `EXPLAIN`'s two numbers, in Postgres, MySQL, Oracle and their
/// kin: the rows a guess (the planner's), the bytes their product.
pub fn planner_estimate(rows: u64, width: Option<u64>, exact_rows: bool) -> quarb::Estimate {
    quarb::Estimate {
        nodes: if exact_rows { (rows, rows) } else { (0, rows) },
        bytes: width.map(|w| (0, rows.saturating_mul(w))),
        requests: Some(1),
        latency: quarb::Latency::Remote,
        confidence: if exact_rows {
            quarb::Confidence::Exact
        } else {
            quarb::Confidence::Guess
        },
    }
}

/// A value as a SQL literal for a keyed `IN (…)`: numbers bare,
/// text quoted with its quotes doubled. `None` for what has no
/// portable literal (a backslash, which some servers read as an
/// escape; booleans, lists, …) — the keyed read is then declined
/// and the table read whole.
fn sql_literal(v: &Value) -> Option<String> {
    match v {
        Value::Int(i) => Some(i.to_string()),
        Value::Float(f) if f.is_finite() => Some(format!("{f:?}")),
        Value::Str(s) if !s.contains('\\') && !s.contains('\0') => {
            Some(format!("'{}'", s.replace('\'', "''")))
        }
        _ => None,
    }
}

struct Row {
    /// The row's edge name: its primary key (or rowid) as text.
    key: String,
    rowid: Option<i64>,
    values: Vec<Value>,
}

/// The pseudo-column of the row key in [`TableData::fetched`].
const KEY: usize = usize::MAX;

/// Keyed statements a table answers before it is read whole: past
/// this many round trips a scan is the cheaper way to go on.
const KEYED_BUDGET: usize = 64;

/// Values per keyed statement (`WHERE col IN (…)`).
const KEYED_CHUNK: usize = 400;

/// A table's rows as far as they have been read.
///
/// Rows live in an arena in order of first reading, and a row's
/// node id is its arena index: it does not change when more rows
/// arrive or when the table is later read whole. The table's own
/// order is kept beside it once known.
#[derive(Default)]
struct TableData {
    rows: Vec<std::rc::Rc<Row>>,
    by_key: HashMap<String, usize>,
    /// The table's own order (arena indices), once read whole.
    order: Option<std::rc::Rc<Vec<usize>>>,
    /// Per column: value text -> the arena indices of the rows
    /// holding it, in the table's order. Under a whole table the
    /// index of a column is complete; before that, exactly the
    /// values in `fetched` are.
    by_col: HashMap<usize, HashMap<String, Vec<usize>>>,
    /// The (column, value) pairs read by key ([`KEY`] for the row
    /// key), found or not.
    fetched: std::collections::HashSet<(usize, String)>,
    /// Keyed statements issued.
    statements: usize,
    /// Keyed reading is off for this table: unsupported, failed, or
    /// over budget.
    keyed_off: bool,
    /// `order` is the table's listing under a pushed-down filter
    /// (see [`RelationalModel::with_filtered_listing`]), not the
    /// whole table: lookups still go by key, and a need for the
    /// whole table reads it.
    filtered: bool,
}

struct LazyTable {
    spec: TableSpec,
    data: std::cell::RefCell<TableData>,
}

/// Node ids pack three levels — table, row, column — so they are
/// stable regardless of load order. The row field is the row's
/// arena index. The column field (low bits) is 0 for a table or
/// row node and `col+1` for a *cell* node (a row's per-column leaf,
/// which carries the column's value as its default projection so
/// composition can graft a JSON string there). The whole id stays
/// below bit 55, the bit `quarb-compose` claims when it grafts an
/// inner arbor onto such a leaf, and below the high byte
/// `quarb-mount` reserves.
const COL_BITS: u64 = 12;
const ROW_BITS: u64 = 32;
const TABLE_SHIFT: u64 = COL_BITS + ROW_BITS;
const COL_MASK: u64 = (1 << COL_BITS) - 1;
const ROW_MASK: u64 = (1 << ROW_BITS) - 1;

/// A database exposed as an arbor: eager or lazily materialized.
pub struct RelationalModel {
    tables: Vec<LazyTable>,
    fetcher: Fetcher,
    key_fetcher: Option<KeyFetcher>,
    /// One table's listing under a pushed-down WHERE: (table index,
    /// reader).
    listing: Option<(usize, Fetcher)>,
    /// Set once the engine asked for whole listings
    /// (`AstAdapter::whole_listings`): the filtered listing is then
    /// bypassed and the table read whole on its next listing.
    listing_off: std::cell::Cell<bool>,
    estimator: Option<Estimator>,
    /// Estimates already asked of the driver, by table and the
    /// equalities' text: a plan is read many times per run.
    estimates: std::cell::RefCell<HashMap<(usize, String), quarb::Estimate>>,
}

/// The driver's estimator: what reading the rows of a table whose
/// columns hold the given values (an empty list: the whole table)
/// would cost, said before reading — a warehouse's dry run, a
/// planner's `EXPLAIN`, the catalog's row count. The model asks it
/// for [`quarb::AstAdapter::estimate`] and remembers the answer for
/// the run. An `Err` leaves the step unknown.
pub type Estimator =
    std::rc::Rc<dyn Fn(usize, &TableSpec, &[(usize, Value)]) -> Result<quarb::Estimate, String>>;

/// An identifier in the double-quoted dialects (SQL standard,
/// Postgres, DuckDB, Athena, SQLite): the quote doubled inside.
pub fn quote_double(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// An identifier in the backtick dialects (MySQL, BigQuery,
/// Cypher): the backtick doubled inside.
pub fn quote_backtick(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn row_of(spec: &TableSpec, r: RowSpec) -> Row {
    let key = match spec.pk {
        Some(i) => r.values[i].to_string(),
        None => r.rowid.unwrap_or_default().to_string(),
    };
    Row {
        key,
        rowid: r.rowid,
        values: r.values,
    }
}

impl RelationalModel {
    /// The fully-eager form: every table's rows supplied up front.
    pub fn build(input: Vec<(TableSpec, Vec<RowSpec>)>) -> Self {
        let model = RelationalModel {
            tables: input
                .iter()
                .map(|(spec, _)| LazyTable {
                    spec: TableSpec {
                        name: spec.name.clone(),
                        columns: spec.columns.clone(),
                        pk: spec.pk,
                        fks: spec.fks.clone(),
                        json_columns: Vec::new(),
                        column_types: spec.column_types.clone(),
                    },
                    data: Default::default(),
                })
                .collect(),
            fetcher: Box::new(|_, spec| {
                unreachable!("eager model refetched table '{}'", spec.name)
            }),
            key_fetcher: None,
            listing: None,
            listing_off: std::cell::Cell::new(false),
            estimator: None,
            estimates: Default::default(),
        };
        for (t, (_, rows)) in input.into_iter().enumerate() {
            model.absorb_whole(t, rows);
        }
        model
    }

    /// The lazy form: the catalog now, each table's rows on first
    /// touch via `fetcher`.
    pub fn lazy(specs: Vec<TableSpec>, fetcher: Fetcher) -> Self {
        RelationalModel {
            tables: specs
                .into_iter()
                .map(|spec| LazyTable {
                    spec,
                    data: Default::default(),
                })
                .collect(),
            fetcher,
            key_fetcher: None,
            listing: None,
            listing_off: std::cell::Cell::new(false),
            estimator: None,
            estimates: Default::default(),
        }
    }

    /// The lazy form for a driver whose one row source takes an
    /// optional WHERE (`fetch`): the whole read, the listing under
    /// a pushed-down filter (`filter`: table and WHERE text, see
    /// [`with_filtered_listing`](Self::with_filtered_listing)), and
    /// the keyed read — `WHERE <quoted column> IN (<literals>)` —
    /// all derive from it. `quote` renders an identifier in the
    /// driver's dialect.
    ///
    /// The keyed read is declined for a table without a
    /// single-column primary key (its rows are named by position,
    /// which a keyed read cannot know) and for a value with no
    /// portable literal; a statement the server rejects (a text
    /// literal against a strictly typed number column) declines it
    /// too. Declined means: that table is read whole, as before.
    /// Keyed rows carry no row id ("position unknown");
    /// `:::rowid` then reads the table to find it.
    pub fn lazy_where(
        specs: Vec<TableSpec>,
        fetch: WhereFetcher,
        filter: Option<(String, String)>,
        quote: fn(&str) -> String,
    ) -> Self {
        let (whole, keyed) = (fetch.clone(), fetch.clone());
        let mut model = RelationalModel::lazy(specs, Box::new(move |t, spec| whole(t, spec, None)))
            .with_key_fetcher(Box::new(move |t, spec, col, values| {
                let Some(pk) = spec.pk else {
                    return Err("rows are named by position".to_string());
                };
                let column = quote(&spec.columns[col.unwrap_or(pk)]);
                let literals: Option<Vec<String>> = values.iter().map(sql_literal).collect();
                let literals = literals.ok_or("a value with no portable SQL literal")?;
                if literals.is_empty() {
                    return Ok(Vec::new());
                }
                let where_sql = format!("{column} IN ({})", literals.join(", "));
                let mut rows = keyed(t, spec, Some(&where_sql))?;
                for r in &mut rows {
                    r.rowid = None;
                }
                Ok(rows)
            }));
        if let Some((table, where_sql)) = filter {
            model = model.with_filtered_listing(
                &table,
                Box::new(move |t, spec| fetch(t, spec, Some(&where_sql))),
            );
        }
        model
    }

    /// Give the lazy model a keyed row source: hops, key lookups
    /// and backlinks then read the rows they need, and a table is
    /// read whole only when a query asks for all of it.
    pub fn with_key_fetcher(mut self, key_fetcher: KeyFetcher) -> Self {
        self.key_fetcher = Some(key_fetcher);
        self
    }

    /// Give the model the driver's estimator: the static plan then
    /// carries what a table read would cost before it runs.
    pub fn with_estimator(mut self, estimator: Estimator) -> Self {
        self.estimator = Some(estimator);
        self
    }

    /// Let `table`'s listing — its rows as children, `/table/*` —
    /// come from `reader`, which fetches them under a WHERE the
    /// planner pushed down (the engine re-applies the predicates,
    /// so the listing may be any superset of the rows the query
    /// keeps). Only the listing is filtered: a hop into the table
    /// still reads its row by key, and anything that needs the
    /// whole table reads it whole, so crosslinks and resolutions
    /// stay exact beside the prefilter. Requires a keyed source
    /// for that guarantee to be cheap; without one the first hop
    /// into the table reads it whole.
    pub fn with_filtered_listing(mut self, table: &str, reader: Fetcher) -> Self {
        if let Some(t) = self.table_index(table) {
            self.listing = Some((t, reader));
        }
        self
    }

    /// The table's rows as its children: under the pushed-down
    /// filter where one is set for it, else the whole table.
    fn listed(&self, t: usize) -> std::rc::Rc<Vec<usize>> {
        // The engine asked for whole listings: a filtered read, made
        // or pending, gives way to the whole table.
        if self.listing_off.get() {
            return self.whole(t);
        }
        if let Some(order) = self.tables[t].data.borrow().order.clone() {
            return order;
        }
        // A table without a single-column primary key names its
        // rows by position, and a filtered listing's positions are
        // not the table's: a later whole read would meet the wrong
        // rows under the same keys. Such a table is read whole (the
        // engine re-applies the pushed predicates anyway), as the
        // keyed read declines it.
        if let Some((lt, reader)) = &self.listing
            && *lt == t
            && self.tables[t].spec.pk.is_some()
        {
            match reader(t, &self.tables[t].spec) {
                Ok(rows) => {
                    self.absorb(t, rows, true);
                    return self.tables[t]
                        .data
                        .borrow()
                        .order
                        .clone()
                        .expect("just absorbed");
                }
                Err(e) => eprintln!(
                    "quarb-relational: filtered read of '{}': {e}; reading it whole",
                    self.tables[t].spec.name
                ),
            }
        }
        self.whole(t)
    }

    /// Take a whole table's rows, in the table's order. Rows read
    /// earlier by key keep their arena places.
    fn absorb_whole(&self, t: usize, rows: Vec<RowSpec>) {
        self.absorb(t, rows, false)
    }

    /// Take a listing of the table — whole, or under a filter — in
    /// the table's order.
    fn absorb(&self, t: usize, rows: Vec<RowSpec>, filtered: bool) {
        let spec = &self.tables[t].spec;
        let mut d = self.tables[t].data.borrow_mut();
        let fresh = d.rows.is_empty();
        let mut order = Vec::with_capacity(rows.len());
        for r in rows {
            let row = row_of(spec, r);
            let at = match (fresh, d.by_key.get(&row.key)) {
                // Rows read by key are found again by their key
                // (keyed tables have unique keys).
                (false, Some(&at)) => at,
                _ => {
                    let at = d.rows.len();
                    d.by_key.insert(row.key.clone(), at);
                    d.rows.push(std::rc::Rc::new(row));
                    at
                }
            };
            order.push(at);
        }
        d.order = Some(std::rc::Rc::new(order));
        d.filtered = filtered;
        if !filtered {
            // The per-column indexes are rebuilt on demand, complete
            // and in the table's order.
            d.by_col.clear();
            d.fetched.clear();
        }
    }

    /// Whether `t` has been read whole.
    fn is_whole(&self, t: usize) -> bool {
        let d = self.tables[t].data.borrow();
        d.order.is_some() && !d.filtered
    }

    /// Whether `t` may still be read by key.
    fn keyed(&self, t: usize) -> bool {
        self.key_fetcher.is_some() && !self.tables[t].data.borrow().keyed_off && !self.is_whole(t)
    }

    /// Force `table`'s rows now, surfacing the fetch error the
    /// lazy path swallows into an empty table — the door for
    /// callers that must fall back rather than answer empty (a
    /// partial-pushdown prefilter the server rejects must send
    /// the caller to the scan). A no-op once materialized.
    pub fn prefetch(&self, table: &str) -> Result<(), String> {
        let t = self
            .tables
            .iter()
            .position(|tb| tb.spec.name == table)
            .ok_or_else(|| format!("no table named {table}"))?;
        if self.tables[t].data.borrow().order.is_some() {
            return Ok(());
        }
        // A table without a single-column primary key names its
        // rows by position, and a filtered listing's positions are
        // not the table's: a later whole read would meet the wrong
        // rows under the same keys. Such a table is read whole (the
        // engine re-applies the pushed predicates anyway), as the
        // keyed read declines it.
        if let Some((lt, reader)) = &self.listing
            && *lt == t
            && self.tables[t].spec.pk.is_some()
        {
            let rows = reader(t, &self.tables[t].spec)?;
            self.absorb(t, rows, true);
            return Ok(());
        }
        let rows = (self.fetcher)(t, &self.tables[t].spec)?;
        self.absorb_whole(t, rows);
        Ok(())
    }

    /// The table's rows in its own order, reading it whole on first
    /// need.
    fn whole(&self, t: usize) -> std::rc::Rc<Vec<usize>> {
        {
            let d = self.tables[t].data.borrow();
            if let (Some(order), false) = (&d.order, d.filtered) {
                return order.clone();
            }
        }
        let table = &self.tables[t];
        // A failed whole read is refused (the query errors) rather
        // than answered as an empty table.
        let rows = quarb::fetched((self.fetcher)(t, &table.spec), || {
            format!("loading table '{}'", table.spec.name)
        });
        self.absorb_whole(t, rows);
        self.tables[t]
            .data
            .borrow()
            .order
            .clone()
            .expect("just absorbed")
    }

    /// One row of the arena.
    fn row_at(&self, t: usize, r: usize) -> Option<std::rc::Rc<Row>> {
        self.tables[t].data.borrow().rows.get(r).cloned()
    }

    /// Read by key the rows of `t` whose column `col` ([`KEY`] for
    /// the row key) holds one of `values`, unless they are known
    /// already. Returns false when the table had to be (or already
    /// was) read whole instead.
    fn fetch_keyed(&self, t: usize, col: usize, values: &[Value]) -> bool {
        if !self.keyed(t) {
            self.whole(t);
            return false;
        }
        let mut wanted: Vec<(String, Value)> = Vec::new();
        {
            let d = self.tables[t].data.borrow();
            for v in values {
                if matches!(v, Value::Null) {
                    continue;
                }
                let text = v.to_string();
                if d.fetched.contains(&(col, text.clone()))
                    || wanted.iter().any(|(w, _)| *w == text)
                    // a row already here by its key is the row
                    || (col == KEY && d.by_key.contains_key(&text))
                {
                    continue;
                }
                wanted.push((text, v.clone()));
            }
        }
        if wanted.is_empty() {
            return true;
        }
        let spec = &self.tables[t].spec;
        let fetcher = self.key_fetcher.as_ref().expect("keyed() checked");
        for chunk in wanted.chunks(KEYED_CHUNK) {
            let over = {
                let mut d = self.tables[t].data.borrow_mut();
                d.statements += 1;
                d.statements > KEYED_BUDGET
            };
            let vals: Vec<Value> = chunk.iter().map(|(_, v)| v.clone()).collect();
            let fetched = if over {
                Err("keyed budget spent".to_string())
            } else {
                fetcher(t, spec, (col != KEY).then_some(col), &vals)
            };
            let rows = match fetched {
                Ok(rows) => rows,
                Err(_) => {
                    self.tables[t].data.borrow_mut().keyed_off = true;
                    self.whole(t);
                    return false;
                }
            };
            let mut d = self.tables[t].data.borrow_mut();
            let mut at_of: Vec<usize> = Vec::with_capacity(rows.len());
            for r in rows {
                let row = row_of(spec, r);
                let at = match d.by_key.get(&row.key) {
                    Some(&at) => at,
                    None => {
                        let at = d.rows.len();
                        d.by_key.insert(row.key.clone(), at);
                        d.rows.push(std::rc::Rc::new(row));
                        at
                    }
                };
                at_of.push(at);
            }
            if col != KEY {
                // Each value's rows, in the order the statement
                // returned them — the table's own.
                let mut lists: HashMap<String, Vec<usize>> = HashMap::new();
                for &at in &at_of {
                    let text = d.rows[at].values[col].to_string();
                    lists.entry(text).or_default().push(at);
                }
                let index = d.by_col.entry(col).or_default();
                for (text, _) in chunk {
                    match lists.remove(text) {
                        Some(list) => {
                            index.insert(text.clone(), list);
                        }
                        None => {
                            index.remove(text);
                        }
                    }
                }
            }
            for (text, _) in chunk {
                d.fetched.insert((col, text.clone()));
            }
        }
        true
    }

    /// The arena index of the row of `t` named `key`, read by key
    /// when the table is not whole.
    fn key_lookup(&self, t: usize, key: &str, value: &Value) -> Option<usize> {
        if let Some(&at) = self.tables[t].data.borrow().by_key.get(key) {
            return Some(at);
        }
        if self.is_whole(t) {
            return None;
        }
        self.fetch_keyed(t, KEY, std::slice::from_ref(value));
        self.tables[t].data.borrow().by_key.get(key).copied()
    }

    /// The arena indices of the rows of `t` whose column `col`
    /// holds `value` (by its text), in the table's order — read by
    /// key when the table is not whole.
    fn col_lookup(&self, t: usize, col: usize, value: &Value) -> Vec<usize> {
        let text = value.to_string();
        if !self.is_whole(t) && self.fetch_keyed(t, col, std::slice::from_ref(value)) {
            let d = self.tables[t].data.borrow();
            return d
                .by_col
                .get(&col)
                .and_then(|ix| ix.get(&text))
                .cloned()
                .unwrap_or_default();
        }
        // Whole: the column's complete index, built on first use.
        let order = self.whole(t);
        let mut d = self.tables[t].data.borrow_mut();
        if !d.by_col.contains_key(&col) {
            let mut map: HashMap<String, Vec<usize>> = HashMap::new();
            for &at in order.iter() {
                let v = &d.rows[at].values[col];
                if matches!(v, Value::Null) {
                    continue;
                }
                map.entry(v.to_string()).or_default().push(at);
            }
            d.by_col.insert(col, map);
        }
        d.by_col[&col].get(&text).cloned().unwrap_or_default()
    }

    fn table_node(t: usize) -> NodeId {
        NodeId((t as u64 + 1) << TABLE_SHIFT)
    }

    fn row_node(t: usize, r: usize) -> NodeId {
        NodeId((t as u64 + 1) << TABLE_SHIFT | (r as u64 + 1) << COL_BITS)
    }

    fn cell_node(t: usize, r: usize, c: usize) -> NodeId {
        NodeId((t as u64 + 1) << TABLE_SHIFT | (r as u64 + 1) << COL_BITS | (c as u64 + 1))
    }

    /// Decode a table or row node: `(table, Some(row))` for a row,
    /// `(table, None)` for a table node. A cell node (column field
    /// set) is *not* one of these, so this returns `None` for it —
    /// the reference/link machinery that keys on `entry` then
    /// naturally no-ops on cells.
    fn entry(&self, node: NodeId) -> Option<(usize, Option<usize>)> {
        if node.0 & COL_MASK != 0 {
            return None;
        }
        let t = (node.0 >> TABLE_SHIFT).checked_sub(1)? as usize;
        if t >= self.tables.len() {
            return None;
        }
        let r = (node.0 >> COL_BITS) & ROW_MASK;
        Some((t, r.checked_sub(1).map(|r| r as usize)))
    }

    /// Decode a cell node into `(table, row, column)`; `None` for a
    /// table or row node (no column field).
    fn cell(&self, node: NodeId) -> Option<(usize, usize, usize)> {
        let c = (node.0 & COL_MASK).checked_sub(1)? as usize;
        let t = (node.0 >> TABLE_SHIFT).checked_sub(1)? as usize;
        if t >= self.tables.len() {
            return None;
        }
        let r = ((node.0 >> COL_BITS) & ROW_MASK).checked_sub(1)? as usize;
        Some((t, r, c))
    }

    fn row(&self, node: NodeId) -> Option<(&TableSpec, std::rc::Rc<Row>)> {
        let (t, r) = self.entry(node)?;
        let row = self.row_at(t, r?)?;
        Some((&self.tables[t].spec, row))
    }

    fn table_index(&self, table: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.spec.name == table)
    }

    /// The row node of `table` whose key column equals `value`.
    /// An empty target column means the table's key.
    fn find_target(&self, table: &str, column: &str, value: &Value) -> Option<NodeId> {
        let t = self.table_index(table)?;
        if column.is_empty() {
            let at = self.key_lookup(t, &value.to_string(), value)?;
            return Some(Self::row_node(t, at));
        }
        let col = self.tables[t]
            .spec
            .columns
            .iter()
            .position(|c| c == column)?;
        let at = *self.col_lookup(t, col, value).first()?;
        Some(Self::row_node(t, at))
    }

    /// The value a referring column must hold to point at `row` of
    /// table `tt` through a key whose target column is `to` (empty:
    /// the row key).
    fn referred_value(&self, tt: usize, row: &Row, to: &str) -> Option<Value> {
        if to.is_empty() {
            return Some(match self.tables[tt].spec.pk {
                Some(i) => row.values[i].clone(),
                None => row.rowid.map_or(Value::Null, Value::Int),
            });
        }
        let i = self.tables[tt].spec.columns.iter().position(|c| c == to)?;
        Some(row.values[i].clone())
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        if let Some((t, r, c)) = self.cell(node) {
            let table = &self.tables[t];
            let col = table.spec.columns.get(c).cloned().unwrap_or_default();
            return match self.row_at(t, r) {
                Some(row) => format!("/{}/{}/{}", table.spec.name, row.key, col),
                None => format!("/{}/{}", table.spec.name, col),
            };
        }
        match self.entry(node) {
            None => "/".to_string(),
            Some((t, None)) => format!("/{}", self.tables[t].spec.name),
            Some((t, Some(r))) => {
                let table = &self.tables[t];
                match self.row_at(t, r) {
                    Some(row) => format!("/{}/{}", table.spec.name, row.key),
                    None => format!("/{}", table.spec.name),
                }
            }
        }
    }
}

impl AstAdapter for RelationalModel {
    fn whole_listings(&self) {
        self.listing_off.set(true);
    }
    /// The kind first (spec: Traits): `database`, `table`, `row`,
    /// `cell`; a cell then carries its column's declared type and
    /// the derived kind (spec: adapter traits).
    fn traits(&self, node: NodeId) -> Vec<String> {
        if let Some((t, _, c)) = self.cell(node) {
            let mut out = vec!["cell".to_string()];
            if let Some(d) = self.tables[t].spec.column_types.get(c) {
                out.extend(type_traits(d));
            }
            return out;
        }
        match self.entry(node) {
            Some((_, Some(_))) => vec!["row".to_string()],
            Some((_, None)) => vec!["table".to_string()],
            None => vec!["database".to_string()],
        }
    }

    fn declared_notation(&self, node: NodeId) -> Option<String> {
        // a cell of a column typed json/jsonb declares JSON
        let (t, _, c) = self.cell(node)?;
        self.tables[t]
            .spec
            .json_columns
            .contains(&c)
            .then(|| "json".to_string())
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            return (0..self.tables.len()).map(Self::table_node).collect();
        }
        match self.entry(node) {
            Some((t, None)) => self
                .listed(t)
                .iter()
                .map(|&r| Self::row_node(t, r))
                .collect(),
            // A row's children are its cells — one leaf per
            // column, mirroring dual exposure: every scalar field
            // answers as a child node and as a property. (String
            // cells additionally let composition graft a JSON
            // document in place.) NULL cells stay properties only,
            // as an absent child is how "no value" navigates.
            Some((t, Some(r))) => {
                let Some(row) = self.row_at(t, r) else {
                    return Vec::new();
                };
                (0..row.values.len())
                    .filter(|&c| !matches!(row.values[c], Value::Null))
                    .map(|c| Self::cell_node(t, r, c))
                    .collect()
            }
            None => Vec::new(),
        }
    }

    /// A child by name without the scan: a table by its name, a row
    /// by its key (`/books/7` reads one row, not the table), a cell
    /// by its column.
    /// A child hop under a table with one leading equality on a
    /// column (`/invoices/*[::customer = "Emma"]`): the rows holding
    /// the value, read by key when the table is not whole, in the
    /// table's order — the keyed read the model already makes for
    /// a resolution, offered to the plan.
    fn select(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<quarb::Selection> {
        let Some((t, None)) = self.entry(from) else {
            return quarb::adapter::select_by_hooks(self, from, plan);
        };
        if plan.axis != quarb::PlanAxis::Child || plan.name.is_some() {
            return quarb::adapter::select_by_hooks(self, from, plan);
        }
        let [quarb::Conjunct::Equals(prop, value)] = plan.conjuncts.as_slice() else {
            return None;
        };
        // Only a text or integer value compares as the engine does
        // under the keyed read; anything else walks.
        if !matches!(value, Value::Str(_) | Value::Int(_)) {
            return None;
        }
        let col = self.tables[t].spec.columns.iter().position(|c| c == prop)?;
        let rows = self.col_lookup(t, col, value);
        Some(quarb::Selection {
            nodes: rows
                .into_iter()
                .map(|r| (Self::row_node(t, r), 1))
                .collect(),
            applied: vec![true],
        })
    }

    /// From the catalog: under a table, a child hop's equality on a
    /// column the table has, with a text or integer value, is the
    /// keyed read's to apply; anything else is the engine's.
    fn select_plan(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<Vec<bool>> {
        let [table] = scope else {
            return None;
        };
        let t = self.table_index(table)?;
        if plan.axis != quarb::PlanAxis::Child || plan.name.is_some() {
            return Some(vec![false; plan.conjuncts.len()]);
        }
        let spec = &self.tables[t].spec;
        Some(
            plan.conjuncts
                .iter()
                .enumerate()
                .map(|(i, c)| match c {
                    quarb::Conjunct::Equals(prop, value) => {
                        i == 0
                            && spec.columns.iter().any(|col| col == prop)
                            && matches!(value, Value::Str(_) | Value::Int(_))
                    }
                    quarb::Conjunct::Compare(..) => false,
                })
                .collect(),
        )
    }

    /// Under a table, a child hop's cost: the driver's estimator
    /// when the model has one (a warehouse's dry run, a planner's
    /// `EXPLAIN`), asked once per run for the table and the
    /// equalities the keyed read would apply; else, a table already
    /// read whole knows its rows exactly and pays nothing more; a
    /// table not yet read with no estimator is unknown.
    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let [table] = scope else {
            return None;
        };
        let t = self.table_index(table)?;
        if plan.axis != quarb::PlanAxis::Child {
            return None;
        }
        // A hop by name under a table is a row by key: at most one,
        // read keyed. The estimator, when there is one, says what
        // that read costs; the rows are clamped to the key's.
        if let Some(name) = plan.name {
            let spec = &self.tables[t].spec;
            let whole = self.is_whole(t);
            let keyed = self.estimator.as_ref().and_then(|est| {
                let pk = spec.pk?;
                est(t, spec, &[(pk, Value::Str(name.to_string()))]).ok()
            });
            return Some(quarb::Estimate {
                nodes: (0, 1),
                bytes: keyed
                    .as_ref()
                    .and_then(|e| e.bytes.map(|b| (0, b.1.min(b.1)))),
                requests: Some(if whole { 0 } else { 1 }),
                latency: keyed.as_ref().map_or(
                    if whole {
                        quarb::Latency::Memory
                    } else {
                        quarb::Latency::Local
                    },
                    |e| e.latency,
                ),
                confidence: quarb::Confidence::Exact,
            });
        }
        if let Some(est) = &self.estimator {
            let spec = &self.tables[t].spec;
            let mut equals: Vec<(usize, Value)> = Vec::new();
            for c in &plan.conjuncts {
                // An equality on a column, text or number, is what a
                // store's statistics price; another comparison
                // leaves the table's count as the bound.
                let (prop, value) = match c {
                    quarb::Conjunct::Equals(p, v) => (p, v),
                    quarb::Conjunct::Compare(p, quarb::Cmp::Eq, v) => (p, v),
                    quarb::Conjunct::Compare(..) => continue,
                };
                let col = spec.columns.iter().position(|c| c == prop)?;
                equals.push((col, (*value).clone()));
            }
            let key = (
                t,
                equals
                    .iter()
                    .map(|(c, v)| format!("{c}={}", v.to_json()))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
            if let Some(e) = self.estimates.borrow().get(&key) {
                return Some(e.clone());
            }
            let e = est(t, spec, &equals).ok()?;
            self.estimates.borrow_mut().insert(key, e.clone());
            return Some(e);
        }
        if !self.is_whole(t) {
            return None;
        }
        let n = self.whole(t).len() as u64;
        let rows = if plan.conjuncts.is_empty() {
            (n, n)
        } else {
            (0, n)
        };
        Some(quarb::Estimate {
            nodes: rows,
            bytes: None,
            requests: Some(0),
            latency: quarb::Latency::Memory,
            confidence: if plan.conjuncts.is_empty() {
                quarb::Confidence::Exact
            } else {
                quarb::Confidence::Bound
            },
        })
    }

    /// `/table/* @| count` on a table already read whole: its length.
    fn count(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<usize> {
        let Some((t, None)) = self.entry(from) else {
            return None;
        };
        if plan.axis != quarb::PlanAxis::Child
            || plan.name.is_some()
            || !plan.conjuncts.is_empty()
            || !self.is_whole(t)
        {
            return None;
        }
        Some(self.whole(t).len())
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        if node.0 == 0 {
            return self
                .table_index(name)
                .map(Self::table_node)
                .into_iter()
                .collect();
        }
        match self.entry(node) {
            Some((t, None)) => {
                if self.keyed(t) || self.tables[t].data.borrow().by_key.contains_key(name) {
                    // The name as the key value: a number where it
                    // reads as one, so an integer key compares as
                    // such on a typed server.
                    let value = match name.parse::<i64>() {
                        Ok(i) if i.to_string() == name => Value::Int(i),
                        _ => Value::Str(name.to_string()),
                    };
                    if let Some(at) = self.key_lookup(t, name, &value) {
                        // Row names that repeat (a sheet keyed by a
                        // column with duplicates) are all wanted:
                        // the scan below answers.
                        let d = self.tables[t].data.borrow();
                        if d.order.is_none() || d.filtered || d.by_key.len() == d.rows.len() {
                            return vec![Self::row_node(t, at)];
                        }
                    } else if !self.is_whole(t) {
                        return Vec::new();
                    }
                }
                let order = self.whole(t);
                let d = self.tables[t].data.borrow();
                if d.by_key.len() != d.rows.len() {
                    return order
                        .iter()
                        .filter(|&&r| d.rows[r].key == name)
                        .map(|&r| Self::row_node(t, r))
                        .collect();
                }
                d.by_key
                    .get(name)
                    .map(|&r| Self::row_node(t, r))
                    .into_iter()
                    .collect()
            }
            Some((t, Some(r))) => {
                let Some(c) = self.tables[t].spec.columns.iter().position(|c| c == name) else {
                    return Vec::new();
                };
                match self.row_at(t, r) {
                    Some(row) if !matches!(row.values[c], Value::Null) => {
                        vec![Self::cell_node(t, r, c)]
                    }
                    _ => Vec::new(),
                }
            }
            None => Vec::new(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        if let Some((t, _, c)) = self.cell(node) {
            return self.tables[t].spec.columns.get(c).cloned();
        }
        match self.entry(node) {
            None => None,
            Some((t, None)) => Some(self.tables[t].spec.name.clone()),
            Some((t, Some(r))) => Some(self.row_at(t, r)?.key.clone()),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        if let Some((t, r, _)) = self.cell(node) {
            return Some(Self::row_node(t, r));
        }
        match self.entry(node) {
            None => None,
            Some((_, None)) => Some(NodeId(0)),
            Some((t, Some(_))) => Some(Self::table_node(t)),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let (spec, row) = self.row(node)?;
        let col = spec.columns.iter().position(|c| c == name)?;
        Some(row.values[col].clone())
    }

    /// A cell node projects to its column's value — the hook
    /// composition reads to graft a JSON string as a subtree.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (t, r, c) = self.cell(node)?;
        self.row_at(t, r)?.values.get(c).cloned()
    }

    /// The `:::` keys this adapter answers (`:meta` lists the ones

    /// a node answers).

    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["n-rows", "columns", "loaded", "n-loaded", "table", "rowid"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match self.entry(node) {
            Some((t, None)) => match key {
                "n-rows" => Some(Value::Int(self.whole(t).len() as i64)),
                "columns" => Some(Value::list(
                    self.tables[t]
                        .spec
                        .columns
                        .iter()
                        .map(|c| Value::Str(c.clone()))
                        .collect(),
                )),
                // Whether this table has been read whole (reads do
                // not trigger a load — this is the lazy model's
                // introspection surface); `n-loaded` counts the
                // rows read so far, by key or whole.
                "loaded" => Some(Value::Bool(self.is_whole(t))),
                "n-loaded" => Some(Value::Int(self.tables[t].data.borrow().rows.len() as i64)),
                _ => None,
            },
            Some((t, Some(r))) => match key {
                "table" => Some(Value::Str(self.tables[t].spec.name.clone())),
                // A row read by key from a driver that names rows
                // by position carries no row id yet: its place in
                // the table is its row id, and finding it reads the
                // table.
                "rowid" => Some(Value::Int(match self.row_at(t, r)?.rowid {
                    None => self.whole(t).iter().position(|&at| at == r)? as i64 + 1,
                    Some(id) => id,
                })),
                _ => None,
            },
            None => None,
        }
    }

    /// Follow a reference column to its target row. A declared
    /// foreign key knows its target; the reference label names a target table
    /// for undeclared references (`::owner-->users`, resolved
    /// against that table's key).
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let (t, r) = self.entry(node)?;
        let spec = &self.tables[t].spec;
        let col = spec.columns.iter().position(|c| c == property)?;
        let value = self.row_at(t, r?)?.values[col].clone();
        if matches!(value, Value::Null) {
            return None;
        }
        if let Some(target) = hint {
            // A label that names the table the column's declared key
            // points at keeps the key's target column (a key to a
            // unique column other than the primary one); any other
            // table is matched by its own key.
            let to = spec
                .fks
                .iter()
                .find(|(c, t, _)| *c == col && t == target)
                .map_or("", |(_, _, to)| to.as_str());
            return self.find_target(target, to, &value);
        }
        let (_, target, to) = spec.fks.iter().find(|(c, _, _)| *c == col)?;
        self.find_target(target, to, &value)
    }

    /// Each declared FK column with a non-null value is an outgoing
    /// crosslink, labeled by the column name without its `_id`
    /// suffix ([`TableSpec::fk_labels`]).
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some((t, Some(r))) = self.entry(node) else {
            return Vec::new();
        };
        let Some(row) = self.row_at(t, r) else {
            return Vec::new();
        };
        let spec = &self.tables[t].spec;
        let labels = spec.fk_labels();
        let mut out = Vec::new();
        for (i, (col, target, to)) in spec.fks.iter().enumerate() {
            let value = &row.values[*col];
            if matches!(value, Value::Null) {
                continue;
            }
            if let Some(n) = self.find_target(target, to, value) {
                out.push((labels[i].clone(), n));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The hops a step is about to take from `nodes`, read in one
    /// statement per key: the rows the outgoing keys point at, or
    /// the rows pointing here.
    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        let mut by_table: HashMap<usize, Vec<std::rc::Rc<Row>>> = HashMap::new();
        for &n in nodes {
            if let Some((t, Some(r))) = self.entry(n)
                && let Some(row) = self.row_at(t, r)
            {
                by_table.entry(t).or_default().push(row);
            }
        }
        for (t, rows) in by_table {
            match dir {
                quarb::LinkDir::Out => {
                    for (col, target, to) in &self.tables[t].spec.fks {
                        let Some(tt) = self.table_index(target) else {
                            continue;
                        };
                        if !self.keyed(tt) {
                            continue;
                        }
                        let to_col = if to.is_empty() {
                            KEY
                        } else {
                            match self.tables[tt].spec.columns.iter().position(|c| c == to) {
                                Some(i) => i,
                                None => continue,
                            }
                        };
                        let values: Vec<Value> =
                            rows.iter().map(|r| r.values[*col].clone()).collect();
                        self.fetch_keyed(tt, to_col, &values);
                    }
                }
                quarb::LinkDir::In => {
                    let name = &self.tables[t].spec.name;
                    for s in 0..self.tables.len() {
                        if !self.keyed(s) {
                            continue;
                        }
                        for (col, target, to) in &self.tables[s].spec.fks {
                            if target != name {
                                continue;
                            }
                            let values: Vec<Value> = rows
                                .iter()
                                .filter_map(|r| self.referred_value(t, r, to))
                                .collect();
                            self.fetch_keyed(s, *col, &values);
                        }
                    }
                }
            }
        }
    }

    /// The reverse of `-->` through the declared keys: every row of
    /// a table with a foreign key into this row's table whose key
    /// column (`property`, or any) holds this row's key — the indexed
    /// answer the engine takes before any walk. `None` when no table
    /// declares a key into this one (an undeclared column needs a
    /// scope: `<--(/books/*::author_id)`).
    fn reverse_resolve(
        &self,
        node: NodeId,
        property: Option<&str>,
        hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        let (tt, Some(_)) = self.entry(node)? else {
            return None;
        };
        let target_name = &self.tables[tt].spec.name;
        // A reference label names the table the property resolves
        // into: under another table's name nothing lands here.
        if hint.is_some_and(|h| h != target_name) {
            return Some(Vec::new());
        }
        let declared = self.tables.iter().any(|t| {
            t.spec.fks.iter().any(|(col, target, _)| {
                target == target_name && property.is_none_or(|p| t.spec.columns[*col] == p)
            })
        });
        if !declared {
            return None;
        }
        // Per referring table, in its own order; a row reached
        // through two of its columns counts once. The property
        // names the column, not the edge's label.
        let mut out: Vec<NodeId> = Vec::new();
        for (t, col, n) in self.referrers(node) {
            if property.is_none_or(|p| self.tables[t].spec.columns[col] == p) && !out.contains(&n) {
                out.push(n);
            }
        }
        // Where a table refers here through several columns its
        // rows are merged into the table's order (which needs the
        // table whole).
        for s in 0..self.tables.len() {
            let cols = self.tables[s]
                .spec
                .fks
                .iter()
                .filter(|(col, target, _)| {
                    target == target_name
                        && property.is_none_or(|p| self.tables[s].spec.columns[*col] == p)
                })
                .count();
            if cols > 1 {
                let order = self.whole(s);
                let place: HashMap<usize, usize> =
                    order.iter().enumerate().map(|(i, &at)| (at, i)).collect();
                let key = |n: &NodeId| match self.entry(*n) {
                    Some((t, Some(r))) => (t, place.get(&r).copied().unwrap_or(usize::MAX)),
                    Some((t, None)) => (t, 0),
                    None => (usize::MAX, 0),
                };
                // Stable: only this table's rows move among
                // themselves.
                let mut mine: Vec<NodeId> = out
                    .iter()
                    .copied()
                    .filter(|n| matches!(self.entry(*n), Some((t, _)) if t == s))
                    .collect();
                mine.sort_by_key(key);
                let mut it = mine.into_iter();
                for n in out.iter_mut() {
                    if matches!(self.entry(*n), Some((t, _)) if t == s) {
                        *n = it.next().expect("same count");
                    }
                }
            }
        }
        Some(out)
    }

    /// A scope that is one table's rows, `/books/*`, with a named
    /// column: the rows whose column holds the value that points at
    /// `target`, read by key instead of the table — the candidates
    /// of `t<--(/books/*::author_id)`, which the engine still
    /// tests. `None` hands the scope back to the engine (another
    /// shape of scope, a table already whole, keyed reading off).
    fn index_referrers(
        &self,
        scope: &str,
        property: &str,
        _label: Option<&str>,
        target: NodeId,
    ) -> Option<Vec<NodeId>> {
        let table = scope.strip_prefix('/')?.strip_suffix("/*")?;
        let s = self.table_index(table)?;
        if !self.keyed(s) {
            return None;
        }
        let col = self.tables[s]
            .spec
            .columns
            .iter()
            .position(|c| c == property)?;
        let (tt, Some(tr)) = self.entry(target)? else {
            return None;
        };
        let row = self.row_at(tt, tr)?;
        // The value a referrer holds: the target's key — which is
        // what a label or the target's own container resolves by —
        // and, under a declared key to another of its columns, that
        // column's value. Two different values: the engine's turn.
        let by_key = self.referred_value(tt, &row, "")?;
        let target_name = &self.tables[tt].spec.name;
        if let Some((_, _, to)) = self.tables[s]
            .spec
            .fks
            .iter()
            .find(|(c, t, to)| *c == col && t == target_name && !to.is_empty())
            && self.referred_value(tt, &row, to)?.to_string() != by_key.to_string()
        {
            return None;
        }
        let hits = self.col_lookup(s, col, &by_key);
        if !self.keyed(s) && !self.is_whole(s) {
            return None;
        }
        Some(hits.into_iter().map(|r| Self::row_node(s, r)).collect())
    }

    /// The catalog answers for a row or a table: does the source's
    /// table declare a key into the target's table?
    fn can_refer_to(&self, source: NodeId, target: NodeId) -> Option<bool> {
        let (st, _) = self.entry(source)?;
        let (tt, _) = self.entry(target)?;
        let target_name = &self.tables[tt].spec.name;
        Some(
            self.tables[st]
                .spec
                .fks
                .iter()
                .any(|(_, t, _)| t == target_name),
        )
    }

    /// With no label written, a referrer's column is also read as a
    /// key into the table the asked-about row stands in.
    fn reverse_label(&self, target: NodeId) -> Option<String> {
        match self.entry(target)? {
            (t, Some(_)) => Some(self.tables[t].spec.name.clone()),
            _ => None,
        }
    }

    /// A column with no declared key names its target only under a
    /// reference label (`::owner-->users`).
    fn resolve_needs_label(&self, node: NodeId, property: &str) -> bool {
        let Some((t, Some(_))) = self.entry(node) else {
            return false;
        };
        let spec = &self.tables[t].spec;
        match spec.columns.iter().position(|c| c == property) {
            Some(col) => !spec.fks.iter().any(|(c, _, _)| *c == col),
            None => false,
        }
    }

    /// The source table's key columns into the target's table.
    fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
        let (Some((st, _)), Some((tt, _))) = (self.entry(source), self.entry(target)) else {
            return Vec::new();
        };
        let target_name = &self.tables[tt].spec.name;
        let spec = &self.tables[st].spec;
        spec.fks
            .iter()
            .filter(|(_, t, _)| t == target_name)
            .map(|(c, _, _)| spec.columns[*c].clone())
            .collect()
    }

    /// Which rows point here: every table with a FK into this row's
    /// table contributes the rows whose FK value matches, in the
    /// referring table's order — read by key where the driver can
    /// (`WHERE fk = …`), else from the table's value index.
    /// The backlinks, labeled as the forward crosslinks are
    /// ([`TableSpec::fk_labels`]).
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let mut labels: HashMap<usize, Vec<String>> = HashMap::new();
        self.referrers(node)
            .into_iter()
            .map(|(t, col, n)| {
                let labels = labels
                    .entry(t)
                    .or_insert_with(|| self.tables[t].spec.fk_labels());
                let i = self.tables[t]
                    .spec
                    .fks
                    .iter()
                    .position(|(c, _, _)| *c == col);
                (
                    i.map_or_else(
                        || self.tables[t].spec.columns[col].clone(),
                        |i| labels[i].clone(),
                    ),
                    n,
                )
            })
            .collect()
    }
}

impl RelationalModel {
    /// Every row with a declared key into this row: its table, the
    /// key column's index, and the row — the backlinks before
    /// their labeling.
    fn referrers(&self, node: NodeId) -> Vec<(usize, usize, NodeId)> {
        let Some((tt, Some(tr))) = self.entry(node) else {
            return Vec::new();
        };
        let Some(row) = self.row_at(tt, tr) else {
            return Vec::new();
        };
        let target_name = self.tables[tt].spec.name.clone();
        let mut out = Vec::new();
        for t in 0..self.tables.len() {
            for (col, target, to) in &self.tables[t].spec.fks {
                if *target != target_name {
                    continue;
                }
                let Some(value) = self.referred_value(tt, &row, to) else {
                    continue;
                };
                for r in self.col_lookup(t, *col, &value) {
                    out.push((t, *col, Self::row_node(t, r)));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_quote_with_their_quote_doubled() {
        assert_eq!(quote_double("plain"), "\"plain\"");
        assert_eq!(quote_double("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_backtick("plain"), "`plain`");
        assert_eq!(quote_backtick("a`b"), "`a``b`");
    }

    fn model() -> RelationalModel {
        // one table `orders`, two columns: an int `id` (the key)
        // and a text `data` holding a JSON string.
        let spec = TableSpec {
            name: "orders".into(),
            columns: vec!["id".into(), "data".into()],
            pk: Some(0),
            fks: vec![],
            json_columns: Vec::new(),
            column_types: Vec::new(),
        };
        let rows = vec![
            RowSpec {
                rowid: Some(1),
                values: vec![Value::Int(1), Value::Str("{\"p\":\"hi\"}".into())],
            },
            RowSpec {
                rowid: Some(2),
                values: vec![Value::Int(2), Value::Str("{\"p\":\"lo\"}".into())],
            },
        ];
        RelationalModel::build(vec![(spec, rows)])
    }

    #[test]
    fn text_columns_become_cell_leaves() {
        let m = model();
        let table = RelationalModel::table_node(0);
        let rows = m.children(table);
        assert_eq!(rows.len(), 2);
        // A row's children are its cells — every non-null column,
        // mirroring dual exposure (`id` and `data` both).
        let cells = m.children(rows[0]);
        assert_eq!(cells.len(), 2);
        assert_eq!(m.name(cells[0]).as_deref(), Some("id"));
        let cells = cells[1..].to_vec();
        assert_eq!(m.name(cells[0]).as_deref(), Some("data"));
        // The cell carries its value through default_value (the
        // hook composition reads to graft), and is childless.
        assert_eq!(
            m.default_value(cells[0]),
            Some(Value::Str("{\"p\":\"hi\"}".into()))
        );
        assert!(m.children(cells[0]).is_empty());
        // Its parent is the row; its locator is /table/key/col.
        assert_eq!(m.parent(cells[0]), Some(rows[0]));
        assert_eq!(m.locator(cells[0]), "/orders/1/data");
        // The column is still reachable as a property, unchanged.
        assert_eq!(m.property(rows[0], "data"), m.default_value(cells[0]));
        // A cell decodes back to its (table, row, col); a row does
        // not decode as a cell.
        assert_eq!(m.cell(cells[0]), Some((0, 0, 1)));
        assert_eq!(m.cell(rows[0]), None);
    }
}
