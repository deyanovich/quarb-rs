//! SQLite adapter for the Quarb query engine.
//!
//! A thin catalog driver over the shared relational model
//! (`quarb-relational`): it introspects the schema via PRAGMAs
//! (`table_info`, `foreign_key_list`), streams every user table,
//! maps SQLite's storage classes to values (`NULL` → null,
//! `INTEGER`/`REAL` → numbers, text as itself, blobs as a size
//! placeholder), and hands the result to [`RelationalModel`].
//!
//! The arbor mapping, the foreign-key reference machinery (`~>`
//! chains, `->`/`<-` crosslinks, `<~` reverse resolution, the
//! table-naming hint), and the metadata surface are documented on
//! the shared model.

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use rusqlite::Connection;
use rusqlite::types::ValueRef;

/// An error loading a database.
#[derive(Debug, thiserror::Error)]
pub enum SqliteError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("pushdown plan: {0}")]
    Plan(String),
}

/// A SQLite database, materialized as an arbor.
pub struct SqliteAdapter {
    model: RelationalModel,
}

fn to_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::Int(n),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(t) => Value::Str(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Str(format!("<blob {} bytes>", b.len())),
    }
}

/// Quote an identifier (table or column name) for interpolation into
/// SQL: wrap in double quotes, doubling any embedded quote, so
/// reserved words and special characters survive.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Introspect the schema: every user table's spec, via PRAGMAs.
fn specs(conn: &Connection) -> Result<Vec<TableSpec>, SqliteError> {
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::new();
    for name in names {
        // Columns and the primary key (a single-column pk names the
        // rows; a composite or absent pk falls back to rowid).
        let mut columns = Vec::new();
        let mut pk_cols: Vec<(i64, usize)> = Vec::new();
        {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quote_ident(&name)))?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let col: String = r.get(1)?;
                let pk: i64 = r.get(5)?;
                if pk > 0 {
                    pk_cols.push((pk, columns.len()));
                }
                columns.push(col);
            }
        }
        pk_cols.sort();
        let pk = (pk_cols.len() == 1).then(|| pk_cols[0].1);

        // Declared foreign keys (an omitted target column means the
        // target's primary key).
        let mut fks = Vec::new();
        {
            let mut stmt =
                conn.prepare(&format!("PRAGMA foreign_key_list({})", quote_ident(&name)))?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let target: String = r.get(2)?;
                let from: String = r.get(3)?;
                let to: Option<String> = r.get(4)?;
                if let Some(idx) = columns.iter().position(|c| *c == from) {
                    fks.push((idx, target, to.unwrap_or_default()));
                }
            }
        }
        out.push(TableSpec {
            name,
            columns,
            pk,
            fks,
        });
    }
    Ok(out)
}

/// Stream one table's rows, optionally filtered (partial
/// pushdown: the engine re-applies the pushed predicates).
fn fetch_rows_where(
    conn: &Connection,
    spec: &TableSpec,
    where_sql: Option<&str>,
) -> Result<Vec<RowSpec>, SqliteError> {
    let cols = spec
        .columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let table = quote_ident(&spec.name);
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    // Prefer SQLite's rowid: stable row identity, and the row's edge
    // name for tables without a single-column primary key. WITHOUT
    // ROWID tables have no rowid column, so `SELECT rowid` fails to
    // prepare; fall back to a positional ordinal (below), ordered by
    // the key for determinism.
    let with_rowid = format!("SELECT rowid, {cols} FROM {table}{filter} ORDER BY rowid");
    if let Ok(mut stmt) = conn.prepare(&with_rowid) {
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let rowid: i64 = r.get(0)?;
            let values: Vec<Value> = (0..spec.columns.len())
                .map(|i| to_value(r.get_ref(i + 1).expect("column in range")))
                .collect();
            out.push(RowSpec { rowid, values });
        }
        return Ok(out);
    }
    // WITHOUT ROWID: no rowid column to select or order by. Order by
    // the single-column primary key (which names the rows) when
    // present, else by every column so the ordinal keys are stable.
    let order = match spec.pk {
        Some(i) => quote_ident(&spec.columns[i]),
        None => cols.clone(),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {cols} FROM {table}{filter} ORDER BY {order}"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    let mut rowid = 0i64;
    while let Some(r) = rows.next()? {
        rowid += 1;
        let values: Vec<Value> = (0..spec.columns.len())
            .map(|i| to_value(r.get_ref(i).expect("column in range")))
            .collect();
        out.push(RowSpec { rowid, values });
    }
    Ok(out)
}

impl SqliteAdapter {
    /// Open a database file (read-only): the catalog now, each
    /// table's rows on first touch (the connection stays owned by
    /// the adapter).
    pub fn open(path: &std::path::Path) -> Result<Self, SqliteError> {
        Self::open_impl(path, None)
    }

    /// [`open`], with one table's fetch filtered by a WHERE clause
    /// (partial pushdown; the engine re-applies the predicates).
    pub fn open_filtered(
        path: &std::path::Path,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, SqliteError> {
        Self::open_impl(path, Some((table.to_string(), where_sql.to_string())))
    }

    fn open_impl(
        path: &std::path::Path,
        filter: Option<(String, String)>,
    ) -> Result<Self, SqliteError> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let specs = specs(&conn)?;
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |_, spec| {
                let w = filter
                    .as_ref()
                    .filter(|(t, _)| *t == spec.name)
                    .map(|(_, w)| w.as_str());
                fetch_rows_where(&conn, spec, w).map_err(|e| e.to_string())
            }),
        );
        Ok(SqliteAdapter { model })
    }

    /// Materialize every user table of an open connection, eagerly
    /// (in-memory databases, tests).
    pub fn load(conn: &Connection) -> Result<Self, SqliteError> {
        let mut input = Vec::new();
        for spec in specs(conn)? {
            let rows = fetch_rows_where(conn, &spec, None)?;
            input.push((spec, rows));
        }
        Ok(SqliteAdapter {
            model: RelationalModel::build(input),
        })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// Execute pushed-down SQL directly (read-only): the column names
/// and rows, ordered by `order_table`'s key when one is given (the
/// pushdown contract: row order must match the adapter's document
/// order, which is that key).
/// Whether `cols` covers the primary key or a non-partial UNIQUE
/// index of `table` — the soundness condition for executing a
/// witness-JOIN pushdown (see `raw_query`).
fn unique_key(conn: &Connection, table: &str, cols: &[String]) -> Result<bool, SqliteError> {
    use std::collections::HashSet;
    let want: HashSet<&str> = cols.iter().map(|s| s.as_str()).collect();
    if want.is_empty() {
        return Ok(false);
    }
    // Primary key (pk ordinal > 0 in table_info).
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut pk: HashSet<String> = HashSet::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: String = r.get(1)?;
        let ord: i64 = r.get(5)?;
        if ord > 0 {
            pk.insert(name);
        }
    }
    if !pk.is_empty() && pk.iter().all(|c| want.contains(c.as_str())) {
        return Ok(true);
    }
    // Non-partial UNIQUE indexes.
    let mut stmt = conn.prepare(&format!("PRAGMA index_list({table})"))?;
    let mut uniques: Vec<String> = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: String = r.get(1)?;
        let is_unique: i64 = r.get(2)?;
        let partial: i64 = r.get(4).unwrap_or(0);
        if is_unique == 1 && partial == 0 {
            uniques.push(name);
        }
    }
    for idx in uniques {
        let mut stmt = conn.prepare(&format!("PRAGMA index_info({idx})"))?;
        let mut cols_of: Vec<String> = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            cols_of.push(r.get(2)?);
        }
        if !cols_of.is_empty() && cols_of.iter().all(|c| want.contains(c.as_str())) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn raw_query(
    path: &std::path::Path,
    sql: &str,
    order_table: Option<&str>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), SqliteError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // A witness-JOIN plan is only sound when the ON binds the left
    // table by a unique key (else SQL multiplies rows where
    // Quarb's existential binding does not). Verify against the
    // catalog; refusing sends the caller back to the scan.
    if let Some((table, cols)) = join_left {
        if !unique_key(&conn, table, cols)? {
            return Err(SqliteError::Plan(format!(
                "join ON does not bind {table} by a unique key; \
                 the SQL JOIN would multiply rows"
            )));
        }
    }
    let sql = match order_table {
        Some(t) => {
            let key = specs(&conn)?
                .into_iter()
                .find(|s| s.name == t)
                .and_then(|s| s.pk.map(|i| s.columns[i].clone()))
                .unwrap_or_else(|| "rowid".to_string());
            format!("{sql} ORDER BY {t}.{key}")
        }
        None => sql.to_string(),
    };
    let mut stmt = conn.prepare(&sql)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let n = cols.len();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(
            (0..n)
                .map(|i| to_value(r.get_ref(i).expect("column in range")))
                .collect(),
        );
    }
    Ok((cols, out))
}

impl AstAdapter for SqliteAdapter {
    fn root(&self) -> NodeId {
        self.model.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.model.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.model.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.model.parent(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.model.property(node, name)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.model.metadata(node, key)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.model.resolve(node, property, hint)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.backlinks(node)
    }
}
