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
    #[error("{0}")]
    Plan(String),
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
    (
        v.div_euclid(per_sec),
        (v.rem_euclid(per_sec) * nanos_per) as u32,
    )
}

/// A cell of a column declared `TIMESTAMP WITH TIME ZONE` is a
/// point of the timeline, `Z`; a plain `TIMESTAMP` is wall time and
/// stays written with no offset (ruling 2026-11-10: an instant with
/// no offset is the one an offset can be declared on). The value
/// reference hands both alike, so the column's declared type says.
fn with_zone(v: Value, declared: Option<&String>) -> Value {
    let zoned = declared.is_some_and(|t| {
        let t = t.to_ascii_uppercase();
        t.contains("WITH TIME ZONE") || t.contains("TIMESTAMPTZ")
    });
    match v {
        Value::Instant {
            secs,
            nanos,
            offset_min: None,
            calendar,
        } if zoned => Value::Instant {
            secs,
            nanos,
            offset_min: Some(0),
            calendar,
        },
        other => other,
    }
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
                calendar: Default::default(),
            }
        }
        Ok(ValueRef::Date32(days)) => Value::Instant {
            secs: days as i64 * 86_400,
            nanos: 0,
            offset_min: None,
            calendar: Default::default(),
        },
        // TIME reads as the span from midnight; INTERVAL by DuckDB's
        // month = 30d, day = 24h expansion.
        Ok(ValueRef::Time64(unit, v)) => {
            let (secs, nanos) = secs_nanos(unit, v);
            Value::seconds(secs, nanos)
        }
        Ok(ValueRef::Interval {
            months,
            days,
            nanos,
        }) => {
            let total = (months as i128) * 30 * 86_400 * 1_000_000_000
                + (days as i128) * 86_400 * 1_000_000_000
                + nanos as i128;
            Value::seconds(
                total.div_euclid(1_000_000_000) as i64,
                total.rem_euclid(1_000_000_000) as u32,
            )
        }
        // BLOB (and UUID, handed over as its 16 bytes) as lowercase hex.
        Ok(ValueRef::Blob(b)) => Value::Str(b.iter().map(|byte| format!("{byte:02x}")).collect()),
        // Nested / enum types have no scalar shape here.
        Ok(_) => Value::Null,
        Err(_) => Value::Null,
    }
}

impl DuckdbAdapter {
    /// Hold the crosslinks of these labels in memory (the mount's
    /// `?crosslinks=order,customer`): see
    /// [`RelationalModel::hold_crosslinks`].
    pub fn hold_crosslinks(&mut self, labels: &[String]) -> Result<(), String> {
        self.model.hold_crosslinks(labels)
    }

    /// Open a `.duckdb` file read-only; the catalog now, each
    /// table's rows on first touch.
    pub fn open(path: &std::path::Path) -> Result<Self, DuckdbError> {
        Self::open_impl(path, None)
    }

    /// [`open`], with one table's listing filtered by a WHERE clause
    /// (partial pushdown; the engine re-applies the predicates, and
    /// steps into the table go by key, unfiltered).
    ///
    /// [`open`]: Self::open
    pub fn open_filtered(
        path: &std::path::Path,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, DuckdbError> {
        Self::open_impl(path, Some((table.to_string(), where_sql.to_string())))
    }

    /// Surface a filtered fetch's error now (see
    /// `RelationalModel::prefetch`): the partial-pushdown caller
    /// falls back to the scan instead of answering empty.
    pub fn prefetch(&self, table: &str) -> Result<(), String> {
        self.model.prefetch(table)
    }

    fn open_impl(
        path: &std::path::Path,
        filter: Option<(String, String)>,
    ) -> Result<Self, DuckdbError> {
        let conn = Connection::open_with_flags(
            path,
            duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?,
        )?;
        let specs = introspect(&conn)?;
        // One row source with an optional WHERE: the whole read and
        // the keyed read a step makes (`WHERE key IN (…)`).
        let conn = std::rc::Rc::new(conn);
        let conn2 = conn.clone();
        let conn3 = conn.clone();
        let model = RelationalModel::lazy_where(
            specs,
            std::rc::Rc::new(
                move |_: usize, spec: &TableSpec, w: Option<&str>, k: Option<usize>| {
                    fetch_rows(&conn, spec, w, k).map_err(|e| e.to_string())
                },
            ),
            filter,
            quarb_relational::quote_double,
        )
        // the exact count of a table not read (the pushdown scanning
        // round, 2026-11-06)
        .with_counter(Box::new(move |_, spec| {
            conn3
                .query_row(&format!("SELECT count(*) FROM {}", spec.name), [], |r| {
                    r.get::<_, i64>(0)
                })
                .map(|n| n.max(0) as usize)
                .map_err(|e| e.to_string())
        }))
        // The catalog's own estimate of the table's rows, read
        // without a scan.
        .with_estimator(std::rc::Rc::new(move |_, spec, equals| {
            let n: i64 = conn2
                .query_row(
                    "SELECT estimated_size FROM duckdb_tables() WHERE table_name = ?",
                    [spec.name.as_str()],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            Ok(quarb::Estimate {
                nodes: (0, n.max(0) as u64),
                bytes: None,
                requests: Some(0),
                latency: quarb::Latency::Local,
                confidence: if equals.is_empty() {
                    quarb::Confidence::Bound
                } else {
                    quarb::Confidence::Guess
                },
            })
        }));
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
        let mut json_columns = Vec::new();
        let mut column_types: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT column_name, data_type FROM information_schema.columns \
                 WHERE table_schema = 'main' AND table_name = ? \
                 ORDER BY ordinal_position",
            )?;
            let mut rows = stmt.query([&name])?;
            while let Some(r) = rows.next()? {
                // a JSON column declares its cells' notation (spec:
                // Grafting)
                let declared: String = r.get(1)?;
                if declared.eq_ignore_ascii_case("json") {
                    json_columns.push(columns.len());
                }
                columns.push(r.get::<_, String>(0)?);
                column_types.push(declared);
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
            json_columns,
            column_types,
        });
    }
    Ok(specs)
}

/// Stream one table's rows; order follows the key when one names
/// rows.
fn fetch_rows(
    conn: &Connection,
    spec: &TableSpec,
    where_sql: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<RowSpec>, DuckdbError> {
    // a prefix: the first rows in the order the whole read uses
    let cut = limit.map(|k| format!(" LIMIT {k}")).unwrap_or_default();
    let cols = spec
        .columns
        .iter()
        .map(|c| quarb_relational::quote_double(c))
        .collect::<Vec<_>>()
        .join(", ");
    let order = match spec.pk {
        Some(i) => format!(
            " ORDER BY {}",
            quarb_relational::quote_double(&spec.columns[i])
        ),
        None => String::new(),
    };
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {cols} FROM {}{filter}{order}{cut}",
        spec.name
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    let mut i = 0i64;
    while let Some(r) = rows.next()? {
        i += 1;
        out.push(RowSpec {
            rowid: Some(i),
            values: (0..spec.columns.len())
                .map(|c| with_zone(cell(r, c), spec.column_types.get(c)))
                .collect(),
        });
    }
    Ok(out)
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s key when one is given (the pushdown
/// contract: row order must match the adapter's document order).
/// The catalog for a plan's column types: every column of `tables`
/// with whether its type is numeric, and whether it is declared
/// JSON. A sort, a fold and a distinct count are the scan's only
/// over a numeric column, and DuckDB would run them over a text one
/// by its own collation and grouping: the caller asks here first.
pub fn column_catalog(
    path: &std::path::Path,
    tables: &[&str],
) -> Result<Vec<(String, bool, bool, bool)>, DuckdbError> {
    let conn = Connection::open_with_flags(
        path,
        duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?,
    )?;
    let mut catalog = Vec::new();
    for table in tables {
        let mut stmt = conn.prepare(
            "SELECT column_name, data_type FROM information_schema.columns \
             WHERE table_schema = 'main' AND table_name = ? \
             ORDER BY ordinal_position",
        )?;
        let mut rows = stmt.query([table])?;
        while let Some(r) = rows.next()? {
            let name: String = r.get(0)?;
            let declared: String = r.get(1)?;
            let d = declared.to_ascii_uppercase();
            // the integer, floating and decimal types (DECIMAL(18,3)
            // carries its parameters in the name)
            let base = d.split('(').next().unwrap_or("").trim();
            let numeric = matches!(
                base,
                "TINYINT"
                    | "SMALLINT"
                    | "INTEGER"
                    | "BIGINT"
                    | "HUGEINT"
                    | "UTINYINT"
                    | "USMALLINT"
                    | "UINTEGER"
                    | "UBIGINT"
                    | "UHUGEINT"
                    | "FLOAT"
                    | "DOUBLE"
                    | "DECIMAL"
            );
            // text compares by bytes (a column declared under a
            // collation is not looked for)
            catalog.push((name, numeric, d == "JSON", true));
        }
    }
    Ok(catalog)
}

/// A plan's float sums in the form that adds in key order (0.71).
/// DuckDB adds a float column plainly and across its threads: the
/// last digits of a sum move from run to run. `fsum(x ORDER BY key)`
/// is its compensated sum in the order the scan reads the rows: the
/// same number every run, and on ordinary columns the engine's
/// careful sum to the last digit (the two methods part on a column
/// of huge cancelling terms). A four-byte column is widened first.
/// `None` for a column that is no float, where SUM is exact; a table
/// with no single-column key keeps the plain sum.
pub fn float_sum_forms(
    path: &std::path::Path,
    sums: &[quarb_relational::FloatSum<'_>],
) -> Result<Vec<Option<String>>, DuckdbError> {
    let conn = Connection::open_with_flags(
        path,
        duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?,
    )?;
    let specs = introspect(&conn)?;
    let mut forms = Vec::new();
    for s in sums {
        let data_type: Option<String> = conn
            .query_row(
                "SELECT data_type FROM information_schema.columns \
                 WHERE table_name = ? AND column_name = ?",
                [s.table, s.column],
                |r| r.get(0),
            )
            .ok();
        let value = match data_type.as_deref() {
            Some("DOUBLE") => s.column_sql.to_string(),
            Some("FLOAT" | "REAL") => format!("({})::DOUBLE", s.column_sql),
            _ => {
                forms.push(None);
                continue;
            }
        };
        let key = specs
            .iter()
            .find(|t| t.name == s.table)
            .and_then(|t| t.pk.map(|i| t.columns[i].clone()));
        forms.push(match key {
            Some(k) => Some(format!(
                "{}({value} ORDER BY \"{}\".\"{k}\")",
                if s.mean { "favg" } else { "fsum" },
                s.table
            )),
            None if value != s.column_sql => {
                Some(format!("{}({value})", if s.mean { "avg" } else { "sum" }))
            }
            None => None,
        });
    }
    Ok(forms)
}

pub fn raw_query(
    path: &std::path::Path,
    sql: &str,
    order: &quarb_relational::PlanOrder<'_>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), DuckdbError> {
    // Witness-JOIN plans carry a uniqueness obligation this
    // driver does not yet verify against its catalog; decline
    // so the caller falls back to the (sound) scan.
    if join_left.is_some() {
        return Err(DuckdbError::Plan(
            "witness-JOIN uniqueness not verified by this driver".into(),
        ));
    }

    let conn = Connection::open_with_flags(
        path,
        duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?,
    )?;
    // The plan's order: its own sort where it has one, then the
    // primary keys the whole reads are ordered by. A plan that needs
    // a key a table lacks could not keep the scan's order, and
    // declines.
    let sql = match order.table {
        Some(_) => {
            let specs = introspect(&conn)?;
            let key_of = |name: &str| {
                specs
                    .iter()
                    .find(|s| s.name == name)
                    .and_then(|s| s.pk.map(|i| s.columns[i].clone()))
            };
            let clause =
                quarb_relational::order_clause(order, &key_of, &|t, k| format!("\"{t}\".\"{k}\""))
                    .map_err(DuckdbError::Plan)?;
            format!("{sql}{clause}")
        }
        None => sql.to_string(),
    };
    // --explain prints what ran: the ORDER BY key is a
    // catalog lookup made here, so the final statement
    // exists nowhere earlier.
    quarb_relational::record_executed(&sql);
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
    // Every trait method, through the relational model: the search
    // fast paths, the plan and its estimate included.
    quarb::forward_ast_adapter!(model);
}
