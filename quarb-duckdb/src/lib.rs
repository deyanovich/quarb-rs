//! DuckDB adapter for the Quarb query engine.
//!
//! The fifth catalog driver over the shared relational model —
//! and the side door to the columnar world: DuckDB reads Parquet
//! and CSV natively, so a `CREATE VIEW ... AS SELECT * FROM
//! read_parquet('data.parquet')` makes columnar files first-class
//! Quarb inputs through this adapter.
//!
//! Catalog from `information_schema` (tables and views, columns,
//! declared PRIMARY KEY / FOREIGN KEY constraints — DuckDB
//! records them), rows lazy per table, native decoding for the
//! numeric/text families with text fallback. `raw_query` serves
//! the pushdown ladder: DuckDB is the fastest scan engine in the
//! fleet, so pushed queries fly. Read-only (the connection opens
//! `ACCESS_MODE=READ_ONLY`).

use duckdb::Connection;
use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};

/// An error opening or loading a database.
#[derive(Debug, thiserror::Error)]
pub enum DuckdbError {
    #[error("duckdb: {0}")]
    Duckdb(#[from] duckdb::Error),
}

/// A DuckDB database, exposed as an arbor.
pub struct DuckdbAdapter {
    model: RelationalModel,
}

/// Split a `TimeUnit`-scaled count into whole seconds and the
/// non-negative sub-second nanosecond remainder (so pre-epoch values
/// floor toward the earlier second, matching instant/duration nanos).
fn secs_nanos(unit: duckdb::types::TimeUnit, v: i64) -> (i64, u32) {
    use duckdb::types::TimeUnit;
    let per_sec: i64 = match unit {
        TimeUnit::Second => 1,
        TimeUnit::Millisecond => 1_000,
        TimeUnit::Microsecond => 1_000_000,
        TimeUnit::Nanosecond => 1_000_000_000,
    };
    let nanos_per = 1_000_000_000 / per_sec;
    (v.div_euclid(per_sec), (v.rem_euclid(per_sec) * nanos_per) as u32)
}

fn cell(row: &duckdb::Row<'_>, i: usize) -> Value {
    use duckdb::types::ValueRef;
    match row.get_ref(i) {
        Ok(ValueRef::Null) => Value::Null,
        Ok(ValueRef::Boolean(b)) => Value::Bool(b),
        Ok(ValueRef::TinyInt(n)) => Value::Int(n as i64),
        Ok(ValueRef::SmallInt(n)) => Value::Int(n as i64),
        Ok(ValueRef::Int(n)) => Value::Int(n as i64),
        Ok(ValueRef::BigInt(n)) => Value::Int(n),
        Ok(ValueRef::UTinyInt(n)) => Value::Int(n as i64),
        Ok(ValueRef::USmallInt(n)) => Value::Int(n as i64),
        Ok(ValueRef::UInt(n)) => Value::Int(n as i64),
        Ok(ValueRef::UBigInt(n)) => Value::Int(n as i64),
        Ok(ValueRef::Float(f)) => Value::Float(f as f64),
        Ok(ValueRef::Double(f)) => Value::Float(f),
        Ok(ValueRef::Text(t)) => Value::Str(String::from_utf8_lossy(t).into_owned()),
        // The families `as_str()` never covers — decoded natively
        // rather than falling through to null. A 128-bit integer
        // narrows when it fits and keeps its decimal text when it
        // doesn't (i64 can't hold every HUGEINT).
        Ok(ValueRef::HugeInt(n)) => match i64::try_from(n) {
            Ok(v) => Value::Int(v),
            Err(_) => Value::Str(n.to_string()),
        },
        // DECIMAL joins the numeric family as a float (quarb has no
        // fixed-point scalar); the exact text is the fallback if it
        // somehow won't parse.
        Ok(ValueRef::Decimal(d)) => {
            let s = d.to_string();
            match s.parse::<f64>() {
                Ok(f) => Value::Float(f),
                Err(_) => Value::Str(s),
            }
        }
        // TIMESTAMP / DATE land on the UTC timeline; a DATE is a
        // midnight instant with no offset, so it prints as a bare date.
        Ok(ValueRef::Timestamp(unit, v)) => {
            let (secs, nanos) = secs_nanos(unit, v);
            Value::Instant {
                secs,
                nanos,
                offset_min: None,
            }
        }
        Ok(ValueRef::Date32(days)) => Value::Instant {
            secs: days as i64 * 86_400,
            nanos: 0,
            offset_min: None,
        },
        // TIME reads as the span from midnight; INTERVAL by DuckDB's
        // month = 30d, day = 24h expansion.
        Ok(ValueRef::Time64(unit, v)) => {
            let (secs, nanos) = secs_nanos(unit, v);
            Value::Duration { secs, nanos }
        }
        Ok(ValueRef::Interval {
            months,
            days,
            nanos,
        }) => {
            let total = (months as i128) * 30 * 86_400 * 1_000_000_000
                + (days as i128) * 86_400 * 1_000_000_000
                + nanos as i128;
            Value::Duration {
                secs: total.div_euclid(1_000_000_000) as i64,
                nanos: total.rem_euclid(1_000_000_000) as u32,
            }
        }
        // BLOB (and UUID, handed over as its 16 bytes) as lowercase hex.
        Ok(ValueRef::Blob(b)) => Value::Str(b.iter().map(|byte| format!("{byte:02x}")).collect()),
        // Nested / enum types have no scalar shape here.
        Ok(_) => Value::Null,
        Err(_) => Value::Null,
    }
}

impl DuckdbAdapter {
    /// Open a `.duckdb` file read-only; the catalog now, each
    /// table's rows on first touch.
    pub fn open(path: &std::path::Path) -> Result<Self, DuckdbError> {
        let conn = Connection::open_with_flags(
            path,
            duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?,
        )?;
        let specs = introspect(&conn)?;
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |_, spec| fetch_rows(&conn, spec).map_err(|e| e.to_string())),
        );
        Ok(DuckdbAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// The catalog: base tables and views of the main schema.
fn introspect(conn: &Connection) -> Result<Vec<TableSpec>, DuckdbError> {
    let mut names = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'main' ORDER BY table_name",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            names.push(r.get::<_, String>(0)?);
        }
    }
    let mut specs = Vec::new();
    for name in names {
        let mut columns = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_schema = 'main' AND table_name = ? \
                 ORDER BY ordinal_position",
            )?;
            let mut rows = stmt.query([&name])?;
            while let Some(r) = rows.next()? {
                columns.push(r.get::<_, String>(0)?);
            }
        }
        // Keys via DuckDB's native constraint catalog
        // (information_schema's constraint_column_usage reports
        // the referencing side for FKs — useless for targets).
        let one_name = |v: &duckdb::types::Value| -> Option<String> {
            match v {
                duckdb::types::Value::List(items) if items.len() == 1 => match &items[0] {
                    duckdb::types::Value::Text(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            }
        };
        let mut pk = None;
        let mut fks = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT constraint_type, constraint_column_names, \
                        referenced_table, referenced_column_names \
                 FROM duckdb_constraints() \
                 WHERE table_name = ? \
                   AND constraint_type IN ('PRIMARY KEY', 'FOREIGN KEY')",
            )?;
            let mut rows = stmt.query([&name])?;
            while let Some(r) = rows.next()? {
                let kind: String = r.get(0)?;
                let cols: duckdb::types::Value = r.get(1)?;
                if kind == "PRIMARY KEY" {
                    if let Some(c) = one_name(&cols) {
                        pk = columns.iter().position(|x| *x == c);
                    }
                } else {
                    let target: String = r.get(2)?;
                    let rcols: duckdb::types::Value = r.get(3)?;
                    if let (Some(from), Some(to)) = (one_name(&cols), one_name(&rcols))
                        && let Some(idx) = columns.iter().position(|x| *x == from)
                    {
                        fks.push((idx, target, to));
                    }
                }
            }
        }
        specs.push(TableSpec {
            name,
            columns,
            pk,
            fks,
        });
    }
    Ok(specs)
}

/// Stream one table's rows; order follows the key when one names
/// rows.
fn fetch_rows(conn: &Connection, spec: &TableSpec) -> Result<Vec<RowSpec>, DuckdbError> {
    let cols = spec
        .columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let order = match spec.pk {
        Some(i) => format!(" ORDER BY \"{}\"", spec.columns[i]),
        None => String::new(),
    };
    let mut stmt = conn.prepare(&format!("SELECT {cols} FROM \"{}\"{order}", spec.name))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    let mut i = 0i64;
    while let Some(r) = rows.next()? {
        i += 1;
        out.push(RowSpec {
            rowid: i,
            values: (0..spec.columns.len()).map(|c| cell(r, c)).collect(),
        });
    }
    Ok(out)
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s key when one is given (the pushdown
/// contract: row order must match the adapter's document order).
pub fn raw_query(
    path: &std::path::Path,
    sql: &str,
    order_table: Option<&str>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), DuckdbError> {
    let conn = Connection::open_with_flags(
        path,
        duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?,
    )?;
    let sql = match order_table {
        Some(t) => {
            let key = introspect(&conn)?
                .into_iter()
                .find(|s| s.name == t)
                .and_then(|s| s.pk.map(|i| s.columns[i].clone()));
            match key {
                Some(k) => format!("{sql} ORDER BY \"{t}\".\"{k}\""),
                None => sql.to_string(),
            }
        }
        None => sql.to_string(),
    };
    let mut stmt = conn.prepare(&sql)?;
    // duckdb's column metadata materializes on execution.
    let mut rows = stmt.query([])?;
    let mut out: Vec<Vec<Value>> = Vec::new();
    let mut n = 0;
    while let Some(r) = rows.next()? {
        if n == 0 {
            n = r.as_ref().column_count();
        }
        out.push((0..n).map(|i| cell(r, i)).collect());
    }
    drop(rows);
    let cols: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    Ok((cols, out))
}

impl AstAdapter for DuckdbAdapter {
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
