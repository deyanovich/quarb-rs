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
//! The arbor mapping and the foreign-key reference machinery (`~>`
//! chains, `->`/`<-` crosslinks, `<~` reverse resolution, the
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
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |t, spec| {
                let w = filter
                    .as_ref()
                    .filter(|(tn, _)| *tn == spec.name)
                    .map(|(_, w)| w.as_str());
                rt.block_on(Self::fetch_rows(&client, spec, &types[t], w))
                    .map_err(|e| e.to_string())
            }),
        );
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
                    "SELECT column_name, data_type \
                     FROM information_schema.columns \
                     WHERE table_schema = 'public' AND table_name = $1 \
                     ORDER BY ordinal_position",
                    &[&name],
                )
                .await?;
            let columns: Vec<String> = cols.iter().map(|r| r.get(0)).collect();
            let types: Vec<String> = cols.iter().map(|r| r.get(1)).collect();

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
    ) -> Result<Vec<RowSpec>, tokio_postgres::Error> {
        let select: Vec<String> = spec
            .columns
            .iter()
            .zip(types)
            .map(|(c, t)| {
                if native_type(t) {
                    format!("\"{c}\"")
                } else {
                    format!("\"{c}\"::text")
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
        let rows = client
            .query(
                &format!(
                    "SELECT {} FROM \"{}\"{filter}{order}",
                    select.join(", "),
                    spec.name
                ),
                &[],
            )
            .await?;
        rows.iter()
            .enumerate()
            .map(|(i, r)| {
                let values = (0..spec.columns.len())
                    .map(|c| cell(r, c))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(RowSpec {
                    rowid: i as i64 + 1,
                    values,
                })
            })
            .collect()
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s key when one is given (the pushdown
/// contract: row order must match the adapter's document order).
pub fn raw_query(
    config: &str,
    sql: &str,
    order_table: Option<&str>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), PostgresError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(config, NoTls).await?;
        tokio::spawn(connection);
        let sql = match order_table {
            Some(t) => {
                let (specs, _) = PostgresAdapter::introspect(&client).await?;
                let key = specs
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
