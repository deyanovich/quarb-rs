//! PostgreSQL adapter for the Quarb query engine.
//!
//! A thin catalog driver over the shared relational model
//! (`quarb-relational`): it connects with `tokio-postgres` (on an
//! internal current-thread runtime, so the public surface stays
//! synchronous), introspects the `public` schema through the
//! system catalogs (tables, columns, primary keys, foreign
//! keys), streams every table, and hands the result to
//! [`RelationalModel`].
//!
//! Type mapping: booleans, the integer family, and the float
//! family arrive natively; text arrives as itself; every other
//! type (numeric, dates, timestamps, uuid, json, arrays, …) is
//! selected with a `::text` cast, where Quarb's numeric reading
//! and comparisons take over — the same posture as the CSV
//! adapter. `NULL` is null.
//!
//! The arbor mapping and the foreign-key reference machinery (`-->`
//! chains, `->`/`<-` crosslinks, `<--` reverse resolution, the
//! table-naming hint) are documented on the shared model. The
//! database is materialized eagerly at connect and opened
//! read-only in spirit (the adapter only ever issues SELECTs).

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use tokio_postgres::types::Type;
use tokio_postgres::{Client, NoTls, Row};

/// An error connecting to or loading a database.
#[derive(Debug, thiserror::Error)]
pub enum PostgresError {
    #[error("postgres: {0}")]
    Postgres(#[from] tokio_postgres::Error),
    #[error("{0}")]
    Plan(String),
    #[error("postgres runtime: {0}")]
    Runtime(#[from] std::io::Error),
}

/// A PostgreSQL database, materialized as an arbor.
pub struct PostgresAdapter {
    model: RelationalModel,
}

/// Whether `data_type` (as `information_schema` spells it) arrives
/// natively; anything else is selected with a `::text` cast.
fn native_type(data_type: &str) -> bool {
    matches!(
        data_type,
        "boolean"
            | "smallint"
            | "integer"
            | "bigint"
            | "real"
            | "double precision"
            | "text"
            | "character varying"
            | "character"
    )
}

fn cell(row: &Row, i: usize) -> Result<Value, tokio_postgres::Error> {
    let ty = row.columns()[i].type_();
    let value = match *ty {
        Type::BOOL => row
            .get::<_, Option<bool>>(i)
            .map_or(Value::Null, Value::Bool),
        Type::INT2 => row
            .get::<_, Option<i16>>(i)
            .map_or(Value::Null, |n| Value::Int(n as i64)),
        Type::INT4 => row
            .get::<_, Option<i32>>(i)
            .map_or(Value::Null, |n| Value::Int(n as i64)),
        Type::INT8 => row.get::<_, Option<i64>>(i).map_or(Value::Null, Value::Int),
        Type::FLOAT4 => row
            .get::<_, Option<f32>>(i)
            .map_or(Value::Null, |f| Value::Float(f as f64)),
        Type::FLOAT8 => row
            .get::<_, Option<f64>>(i)
            .map_or(Value::Null, Value::Float),
        // Non-native types are `::text`-cast on the adapter's own fetch
        // path, so this arm sees a real `text` column there. A full
        // pushdown (`raw_query`) can hand us a raw numeric/date/
        // timestamp/uuid/json column with no cast, though: decoding
        // that as `String` would panic, so use `try_get` and surface
        // the type error to the caller (qua then falls back to the scan
        // path) rather than aborting the process.
        _ => row
            .try_get::<_, Option<String>>(i)?
            .map_or(Value::Null, Value::Str),
    };
    Ok(value)
}

impl PostgresAdapter {
    /// Hold the crosslinks of these labels in memory (the mount's
    /// `crosslinks=order,customer`, written in the URL's query
    /// string): see [`RelationalModel::hold_crosslinks`].
    pub fn hold_crosslinks(&mut self, labels: &[String]) -> Result<(), String> {
        self.model.hold_crosslinks(labels)
    }

    /// Connect and materialize the `public` schema. `config` is a
    /// `tokio-postgres` connection string — URL form
    /// (`postgres://user@host:port/db`) or keyword form
    /// (`host=/run/postgresql user=me dbname=db`).
    pub fn connect(config: &str) -> Result<Self, PostgresError> {
        Self::connect_impl(config, None)
    }

    /// [`connect`], with one table's fetch filtered by a WHERE
    /// clause (partial pushdown; the engine re-applies the
    /// predicates).
    pub fn connect_filtered(
        config: &str,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, PostgresError> {
        Self::connect_impl(config, Some((table.to_string(), where_sql.to_string())))
    }

    fn connect_impl(config: &str, filter: Option<(String, String)>) -> Result<Self, PostgresError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        // Introspect the catalog now; keep the runtime, client, and
        // connection driver alive inside the fetcher so each table's
        // rows can stream on first touch.
        let (client, specs, types) = rt.block_on(async {
            let (client, connection) = tokio_postgres::connect(config, NoTls).await?;
            tokio::spawn(connection);
            let (specs, types) = Self::introspect(&client).await?;
            Ok::<_, tokio_postgres::Error>((client, specs, types))
        })?;
        // One row source with an optional WHERE: the whole read, the
        // listing under a pushed-down filter, and the keyed read a
        // step makes (`WHERE key IN (…)`) all derive from it.
        let rt = std::rc::Rc::new(rt);
        let client = std::rc::Rc::new(client);
        let (rt2, client2) = (rt.clone(), client.clone());
        let (rt3, client3) = (rt.clone(), client.clone());
        let model = RelationalModel::lazy_where(
            specs,
            std::rc::Rc::new(
                move |t: usize, spec: &TableSpec, w: Option<&str>, k: Option<usize>| {
                    rt.block_on(Self::fetch_rows(&client, spec, &types[t], w, k))
                        .map_err(|e| e.to_string())
                },
            ),
            filter,
            quarb_relational::quote_double,
        )
        // the exact count of a table not read (the pushdown scanning
        // round, 2026-11-06)
        .with_counter(Box::new(move |_, spec| {
            rt3.block_on(async {
                let row = client3
                    .query_one(
                        &format!(
                            "SELECT count(*) FROM {}",
                            quarb_relational::quote_double(&spec.name)
                        ),
                        &[],
                    )
                    .await?;
                Ok::<_, tokio_postgres::Error>(row.get::<_, i64>(0).max(0) as usize)
            })
            .map_err(|e| e.to_string())
        }))
        // The planner's estimate, with nothing run: `EXPLAIN` gives
        // the rows it expects and their average width.
        .with_estimator(std::rc::Rc::new(move |_, spec, equals| {
            let sql = format!(
                "EXPLAIN SELECT * FROM {}{}",
                quarb_relational::quote_double(&spec.name),
                quarb_relational::where_suffix(spec, equals, quarb_relational::quote_double)
            );
            let rows = rt2
                .block_on(client2.query(sql.as_str(), &[]))
                .map_err(|e| e.to_string())?;
            let line: String = rows.first().ok_or("an empty plan")?.get(0);
            let n = quarb_relational::plan_number(&line, "rows=").ok_or("no row estimate")?;
            Ok(quarb_relational::planner_estimate(
                n,
                quarb_relational::plan_number(&line, "width="),
                false,
            ))
        }));
        Ok(PostgresAdapter { model })
    }

    /// The catalog: every public table's spec, plus its columns'
    /// catalog types (which decide the `::text` casts at fetch).
    async fn introspect(
        client: &Client,
    ) -> Result<(Vec<TableSpec>, Vec<Vec<String>>), tokio_postgres::Error> {
        let names: Vec<String> = client
            .query(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
                 ORDER BY table_name",
                &[],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();

        let mut specs = Vec::new();
        let mut all_types = Vec::new();
        for name in names {
            // Columns, in ordinal order, with their catalog types.
            let cols = client
                .query(
                    "SELECT column_name, data_type, udt_name \
                     FROM information_schema.columns \
                     WHERE table_schema = 'public' AND table_name = $1 \
                     ORDER BY ordinal_position",
                    &[&name],
                )
                .await?;
            let columns: Vec<String> = cols.iter().map(|r| r.get(0)).collect();
            let types: Vec<String> = cols.iter().map(|r| r.get(1)).collect();
            // The type as PostgreSQL names it (`varchar`, `int4`,
            // `timestamptz`; an array's element type behind its
            // underscore), the cells' type trait.
            let column_types: Vec<String> = cols
                .iter()
                .map(|r| r.get::<_, String>(2).trim_start_matches('_').to_string())
                .collect();
            // a json or jsonb column declares its cells' notation
            // (spec: Grafting)
            let json_columns: Vec<usize> = types
                .iter()
                .enumerate()
                .filter(|(_, t)| t.eq_ignore_ascii_case("json") || t.eq_ignore_ascii_case("jsonb"))
                .map(|(i, _)| i)
                .collect();

            // The primary key (single-column keys name the rows).
            let pk_rows = client
                .query(
                    "SELECT kcu.column_name \
                     FROM information_schema.table_constraints tc \
                     JOIN information_schema.key_column_usage kcu \
                       ON tc.constraint_name = kcu.constraint_name \
                      AND tc.table_schema = kcu.table_schema \
                      AND tc.table_name = kcu.table_name \
                     WHERE tc.table_schema = 'public' AND tc.table_name = $1 \
                       AND tc.constraint_type = 'PRIMARY KEY' \
                     ORDER BY kcu.ordinal_position",
                    &[&name],
                )
                .await?;
            let pk = (pk_rows.len() == 1).then(|| {
                let col: String = pk_rows[0].get(0);
                columns.iter().position(|c| *c == col)
            });
            let pk = pk.flatten();

            // Declared foreign keys. Read straight from `pg_catalog`:
            // `conkey`/`confkey` are parallel column-number arrays, so
            // unnesting them together (`WITH ORDINALITY` keeps them in
            // lockstep) pairs each referencing column with its own
            // referenced column — a composite key stays paired instead
            // of cross-producting. Keying on `conrelid` (the owning
            // table) also avoids the information_schema hazard that
            // constraint names collide across tables.
            let fk_rows = client
                .query(
                    "SELECT a.attname, cf.relname, af.attname \
                     FROM pg_catalog.pg_constraint con \
                     JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_catalog.pg_class cf ON cf.oid = con.confrelid \
                     JOIN LATERAL unnest(con.conkey, con.confkey) \
                          WITH ORDINALITY AS k(conkey, confkey, ord) ON true \
                     JOIN pg_catalog.pg_attribute a \
                       ON a.attrelid = con.conrelid AND a.attnum = k.conkey \
                     JOIN pg_catalog.pg_attribute af \
                       ON af.attrelid = con.confrelid AND af.attnum = k.confkey \
                     WHERE con.contype = 'f' AND n.nspname = 'public' \
                       AND c.relname = $1 \
                     ORDER BY con.oid, k.ord",
                    &[&name],
                )
                .await?;
            let mut fks = Vec::new();
            for r in &fk_rows {
                let from: String = r.get(0);
                let target: String = r.get(1);
                let to: String = r.get(2);
                if let Some(idx) = columns.iter().position(|c| *c == from) {
                    fks.push((idx, target, to));
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
            all_types.push(types);
        }
        Ok((specs, all_types))
    }

    /// Stream one table's rows: native types bare, the rest cast to
    /// text; row order follows the key when one names rows.
    async fn fetch_rows(
        client: &Client,
        spec: &TableSpec,
        types: &[String],
        where_sql: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<RowSpec>, tokio_postgres::Error> {
        let select: Vec<String> = spec
            .columns
            .iter()
            .zip(types)
            .map(|(c, t)| {
                let q = quarb_relational::quote_double(c);
                if native_type(t) {
                    q
                } else {
                    format!("{q}::text")
                }
            })
            .collect();
        // Qualify with the table name so a non-native pk (selected as
        // `"pk"::text`, whose output column is also named `pk`) sorts by
        // the raw column, not the text-cast alias — lexicographic vs.
        // numeric — keeping this in step with `raw_query`'s qualified
        // `ORDER BY "t"."pk"`.
        let order = match spec.pk {
            Some(i) => format!(" ORDER BY \"{}\".\"{}\"", spec.name, spec.columns[i]),
            None => String::new(),
        };
        let filter = match where_sql {
            Some(w) => format!(" WHERE {w}"),
            None => String::new(),
        };
        // a prefix: the first rows in the order the whole read uses
        let cut = limit.map(|k| format!(" LIMIT {k}")).unwrap_or_default();
        // a row cursor: the rows arrive one at a time and are kept
        // only in the model's own shape
        let stream = client
            .query_raw(
                &format!(
                    "SELECT {} FROM {}{filter}{order}{cut}",
                    select.join(", "),
                    quarb_relational::quote_double(&spec.name)
                ),
                std::iter::empty::<&(dyn tokio_postgres::types::ToSql + Sync)>(),
            )
            .await?;
        futures_util::pin_mut!(stream);
        let mut out = Vec::new();
        while let Some(r) = futures_util::TryStreamExt::try_next(&mut stream).await? {
            let values = (0..spec.columns.len())
                .map(|c| cell(&r, c))
                .collect::<Result<Vec<_>, _>>()?;
            let i = out.len() as i64 + 1;
            out.push(RowSpec {
                rowid: Some(i),
                values,
            });
        }
        Ok(out)
    }

    /// A human-readable locator: `/table/key` for rows.
    /// Surface a filtered fetch's error now (see
    /// `RelationalModel::prefetch`): the partial-pushdown caller
    /// falls back to the scan instead of answering empty.
    pub fn prefetch(&self, table: &str) -> Result<(), String> {
        self.model.prefetch(table)
    }

    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s key when one is given (the pushdown
/// contract: row order must match the adapter's document order).
/// The catalog for a plan's column types: every column of `tables`
/// with whether its type is numeric, and whether it is JSON. A sort,
/// a fold and a distinct count are the scan's only over a numeric
/// column, and PostgreSQL would run them over a text one by its own
/// collation and grouping: the caller asks here first.
pub fn column_catalog(
    config: &str,
    tables: &[&str],
) -> Result<Vec<(String, bool, bool, bool)>, PostgresError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(config, NoTls).await?;
        tokio::spawn(connection);
        let mut catalog = Vec::new();
        for table in tables {
            let cols = client
                .query(
                    "SELECT column_name, data_type, udt_name FROM information_schema.columns \
                     WHERE table_schema = 'public' AND table_name = $1 \
                     ORDER BY ordinal_position",
                    &[table],
                )
                .await?;
            for r in &cols {
                let name: String = r.get(0);
                let data_type: String = r.get(1);
                let numeric = matches!(
                    data_type.as_str(),
                    "smallint" | "integer" | "bigint" | "numeric" | "real" | "double precision"
                );
                let json = matches!(data_type.as_str(), "json" | "jsonb");
                // A `char(n)` answers its padding and compares
                // without it ('Mars    ' = 'Mars'); a `citext`
                // compares without its case. Neither compares as the
                // engine reads it.
                let udt: String = r.get(2);
                let exact = data_type != "character" && udt != "citext";
                catalog.push((name, numeric, json, exact));
            }
        }
        Ok(catalog)
    })
}

/// A plan's float sums in the form that adds in key order (0.71).
/// PostgreSQL adds a float column plainly, and under a parallel plan
/// in whatever order its workers return: the last digits of a sum
/// move from run to run. `sum(x ORDER BY key)` adds in the order the
/// scan reads the rows, the same number every run (the engine's own
/// is the careful sum, which may still differ from it in the last
/// digits). A four-byte column is widened first: PostgreSQL would
/// add it in four bytes, the engine reads each cell as a double.
/// `None` for a column that is no float, where SUM is exact; a table
/// with no single-column key keeps the plain sum.
pub fn float_sum_forms(
    config: &str,
    sums: &[quarb_relational::FloatSum<'_>],
) -> Result<Vec<Option<String>>, PostgresError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(config, NoTls).await?;
        tokio::spawn(connection);
        let (specs, _) = PostgresAdapter::introspect(&client).await?;
        let mut forms = Vec::new();
        for s in sums {
            let row = client
                .query_opt(
                    "SELECT data_type FROM information_schema.columns \
                     WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
                    &[&s.table, &s.column],
                )
                .await?;
            let data_type: Option<String> = row.map(|r| r.get(0));
            let value = match data_type.as_deref() {
                Some("double precision") => s.column_sql.to_string(),
                Some("real") => format!("({})::float8", s.column_sql),
                _ => {
                    forms.push(None);
                    continue;
                }
            };
            let key = specs
                .iter()
                .find(|t| t.name == s.table)
                .and_then(|t| t.pk.map(|i| t.columns[i].clone()));
            let f = if s.mean { "avg" } else { "sum" };
            forms.push(match key {
                Some(k) => Some(format!("{f}({value} ORDER BY \"{}\".\"{k}\")", s.table)),
                None if value != s.column_sql => Some(format!("{f}({value})")),
                None => None,
            });
        }
        Ok(forms)
    })
}

pub fn raw_query(
    config: &str,
    sql: &str,
    order: &quarb_relational::PlanOrder<'_>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), PostgresError> {
    // Witness-JOIN plans carry a uniqueness obligation this
    // driver does not yet verify against its catalog; decline
    // so the caller falls back to the (sound) scan.
    if join_left.is_some() {
        return Err(PostgresError::Plan(
            "witness-JOIN uniqueness not verified by this driver".into(),
        ));
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(config, NoTls).await?;
        tokio::spawn(connection);
        // The plan's order: its own sort where it has one, then
        // the primary keys the whole reads are ordered by. A plan
        // that needs a key a table lacks could not keep the scan's
        // order, and declines.
        let sql = match order.table {
            Some(_) => {
                let (specs, _) = PostgresAdapter::introspect(&client).await?;
                let key_of = |name: &str| {
                    specs
                        .iter()
                        .find(|s| s.name == name)
                        .and_then(|s| s.pk.map(|i| s.columns[i].clone()))
                };
                let clause = quarb_relational::order_clause(order, &key_of, &|t, k| {
                    format!("\"{t}\".\"{k}\"")
                })
                .map_err(PostgresError::Plan)?;
                format!("{sql}{clause}")
            }
            None => sql.to_string(),
        };
        // --explain prints what ran: the ORDER BY key is a
        // catalog lookup made here, so the final statement
        // exists nowhere earlier.
        quarb_relational::record_executed(&sql);
        let rows = client.query(&sql, &[]).await?;
        let cols: Vec<String> = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();
        let out = rows
            .iter()
            .map(|r| {
                (0..r.columns().len())
                    .map(|i| cell(r, i))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((cols, out))
    })
}

impl AstAdapter for PostgresAdapter {
    // Every trait method, through the relational model: the search
    // fast paths, the plan and its estimate included.
    quarb::forward_ast_adapter!(model);
}
