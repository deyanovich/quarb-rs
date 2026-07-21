//! MySQL/MariaDB adapter for the Quarb query engine.
//!
//! The third catalog driver over the shared relational model
//! (`quarb-relational`): it connects with `sqlx` (on an internal
//! current-thread runtime, so the public surface stays
//! synchronous), introspects the connected database through
//! `information_schema` — tables, columns with their types, primary
//! keys, and foreign keys (`key_column_usage`'s `referenced_*`
//! columns carry them directly) — and supplies rows lazily, per
//! table on first touch.
//!
//! Type mapping: the integer family arrives as integers, floats
//! and doubles as floats, the character family as text; every
//! other type (decimal, dates, times, json, blobs, …) is selected
//! with a `CAST(... AS CHAR)`, where Quarb's numeric reading and
//! comparisons take over — the same posture as the PostgreSQL and
//! CSV adapters. SQL `NULL` is null.
//!
//! The arbor mapping, the loading model (catalog eager, rows
//! lazy), and the foreign-key reference machinery are documented
//! on the shared model. The adapter only ever issues SELECTs.

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow};
use sqlx::{ConnectOptions, Row};
use std::cell::RefCell;
use std::rc::Rc;
use std::str::FromStr;

/// An error connecting to or loading a database.
#[derive(Debug, thiserror::Error)]
pub enum MysqlError {
    #[error("mysql: {0}")]
    Mysql(#[from] sqlx::Error),
    #[error("pushdown plan: {0}")]
    Plan(String),
    #[error("mysql runtime: {0}")]
    Runtime(#[from] std::io::Error),
}

/// How a column decodes, decided from its catalog type at
/// introspection (cast columns arrive as text).
#[derive(Clone, Copy)]
enum Decode {
    Int,
    Float,
    Text,
}

/// The decode kind for an `information_schema` `data_type`; `None`
/// means the column is selected with a `CAST(... AS CHAR)`.
fn native_decode(data_type: &str) -> Option<Decode> {
    match data_type {
        "tinyint" | "smallint" | "mediumint" | "int" | "bigint" => Some(Decode::Int),
        "float" | "double" => Some(Decode::Float),
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" => Some(Decode::Text),
        _ => None,
    }
}

fn cell(row: &MySqlRow, i: usize, decode: Decode) -> Value {
    match decode {
        Decode::Int => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Int),
        Decode::Float => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Float),
        Decode::Text => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Str),
    }
}

/// Decode one result cell of a pushed-down query, whose columns
/// carry no catalog-driven [`Decode`]: probe int, then float, then
/// text. A NULL decodes as `Ok(None)` at the first probe (the null
/// short-circuits sqlx's type-compat check) and lands as
/// [`Value::Null`]. A non-null value whose type none of the three
/// accept — a DECIMAL/DATE/DATETIME/TIME column on a full pushdown,
/// where no `CAST(... AS CHAR)` was applied — makes every probe
/// `Err`; surface that error rather than silently nulling real data,
/// so the caller falls back to the scan path (which casts such
/// columns to CHAR).
fn raw_cell(row: &MySqlRow, i: usize) -> Result<Value, sqlx::Error> {
    match row.try_get::<Option<i64>, _>(i) {
        Ok(Some(n)) => return Ok(Value::Int(n)),
        Ok(None) => return Ok(Value::Null),
        Err(_) => {}
    }
    match row.try_get::<Option<f64>, _>(i) {
        Ok(Some(f)) => return Ok(Value::Float(f)),
        Ok(None) => return Ok(Value::Null),
        Err(_) => {}
    }
    Ok(row
        .try_get::<Option<String>, _>(i)?
        .map_or(Value::Null, Value::Str))
}

/// A MySQL/MariaDB database, exposed as an arbor.
pub struct MysqlAdapter {
    model: RelationalModel,
}

impl MysqlAdapter {
    /// Connect and introspect. `config` is a `mysql://` URL
    /// (`mysql://user@host:3306/db`, or over a socket:
    /// `mysql://user@localhost/db?socket=/run/mysqld/mysqld.sock`).
    /// Rows load lazily, per table on first touch.
    pub fn connect(config: &str) -> Result<Self, MysqlError> {
        Self::connect_impl(config, None)
    }

    /// [`connect`], with one table's fetch filtered by a WHERE
    /// clause (partial pushdown; the engine re-applies the
    /// predicates).
    pub fn connect_filtered(
        config: &str,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, MysqlError> {
        Self::connect_impl(config, Some((table.to_string(), where_sql.to_string())))
    }

    fn connect_impl(config: &str, filter: Option<(String, String)>) -> Result<Self, MysqlError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let opts = MySqlConnectOptions::from_str(config)?;
        let (conn, specs, decodes) = rt.block_on(async {
            let mut conn = opts.connect().await?;
            let (specs, decodes) = introspect(&mut conn).await?;
            Ok::<_, sqlx::Error>((conn, specs, decodes))
        })?;
        let conn = Rc::new(RefCell::new(conn));
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |t, spec| {
                let mut conn = conn.borrow_mut();
                let w = filter
                    .as_ref()
                    .filter(|(tn, _)| *tn == spec.name)
                    .map(|(_, w)| w.as_str());
                rt.block_on(fetch_rows(&mut conn, spec, &decodes[t], w))
                    .map_err(|e| e.to_string())
            }),
        );
        Ok(MysqlAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// The catalog: every table of the connected database, plus each
/// column's decode kind.
async fn introspect(
    conn: &mut MySqlConnection,
) -> Result<(Vec<TableSpec>, Vec<Vec<Decode>>), sqlx::Error> {
    let names: Vec<String> = sqlx::query(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
         ORDER BY table_name",
    )
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| r.get::<String, _>(0))
    .collect();

    let mut specs = Vec::new();
    let mut all_decodes = Vec::new();
    for name in names {
        let cols = sqlx::query(
            "SELECT column_name, data_type FROM information_schema.columns \
             WHERE table_schema = DATABASE() AND table_name = ? \
             ORDER BY ordinal_position",
        )
        .bind(&name)
        .fetch_all(&mut *conn)
        .await?;
        let columns: Vec<String> = cols.iter().map(|r| r.get(0)).collect();
        let decodes: Vec<Decode> = cols
            .iter()
            .map(|r| {
                let t: String = r.get(1);
                native_decode(&t).unwrap_or(Decode::Text)
            })
            .collect();
        // The primary key (single-column keys name the rows).
        let pk_rows = sqlx::query(
            "SELECT column_name FROM information_schema.key_column_usage \
             WHERE table_schema = DATABASE() AND table_name = ? \
               AND constraint_name = 'PRIMARY' \
             ORDER BY ordinal_position",
        )
        .bind(&name)
        .fetch_all(&mut *conn)
        .await?;
        let pk = (pk_rows.len() == 1)
            .then(|| {
                let col: String = pk_rows[0].get(0);
                columns.iter().position(|c| *c == col)
            })
            .flatten();

        // Declared foreign keys, straight off key_column_usage.
        let fk_rows = sqlx::query(
            "SELECT column_name, referenced_table_name, referenced_column_name \
             FROM information_schema.key_column_usage \
             WHERE table_schema = DATABASE() AND table_name = ? \
               AND referenced_table_name IS NOT NULL",
        )
        .bind(&name)
        .fetch_all(&mut *conn)
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
        all_decodes.push(decodes);
    }
    Ok((specs, all_decodes))
}

/// Stream one table's rows: native columns bare, the rest cast to
/// CHAR; row order follows the key when one names rows.
async fn fetch_rows(
    conn: &mut MySqlConnection,
    spec: &TableSpec,
    decodes: &[Decode],
    where_sql: Option<&str>,
) -> Result<Vec<RowSpec>, sqlx::Error> {
    let select: Vec<String> = spec
        .columns
        .iter()
        .zip(decodes)
        .map(|(c, d)| match d {
            Decode::Int | Decode::Float => format!("`{c}`"),
            // Text covers both native character columns and every
            // cast column; casting a native text column is a no-op.
            Decode::Text => format!("CAST(`{c}` AS CHAR) AS `{c}`"),
        })
        .collect();
    let order = match spec.pk {
        Some(i) => format!(" ORDER BY `{}`", spec.columns[i]),
        None => String::new(),
    };
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    let rows = sqlx::query(&format!(
        "SELECT {} FROM `{}`{filter}{order}",
        select.join(", "),
        spec.name
    ))
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .enumerate()
        .map(|(i, r)| RowSpec {
            rowid: i as i64 + 1,
            values: (0..spec.columns.len())
                .map(|c| cell(r, c, decodes[c]))
                .collect(),
        })
        .collect())
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s key when one is given (the pushdown
/// contract: row order must match the adapter's document order).
pub fn raw_query(
    config: &str,
    sql: &str,
    order_table: Option<&str>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), MysqlError> {
    // Witness-JOIN plans carry a uniqueness obligation this
    // driver does not yet verify against its catalog; decline
    // so the caller falls back to the (sound) scan.
    if join_left.is_some() {
        return Err(MysqlError::Plan(
            "witness-JOIN uniqueness not verified by this driver".into(),
        ));
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let opts = MySqlConnectOptions::from_str(config)?;
    rt.block_on(async {
        let mut conn = opts.connect().await?;
        let sql = match order_table {
            Some(t) => {
                let (specs, _) = introspect(&mut conn).await?;
                let key = specs
                    .into_iter()
                    .find(|s| s.name == t)
                    .and_then(|s| s.pk.map(|i| s.columns[i].clone()));
                match key {
                    Some(k) => format!("{sql} ORDER BY `{t}`.`{k}`"),
                    None => sql.to_string(),
                }
            }
            None => sql.to_string(),
        };
        let rows = sqlx::query(&sql).fetch_all(&mut conn).await?;
        let cols: Vec<String> = rows
            .first()
            .map(|r| {
                use sqlx::Column as _;
                r.columns().iter().map(|c| c.name().to_string()).collect()
            })
            .unwrap_or_default();
        // Without catalog types per result column, decode by probing:
        // int, then float, then text (see [`raw_cell`]). A non-null
        // value none of the three accept surfaces its decode error so
        // the caller falls back to the scan path rather than getting a
        // spurious null.
        let out = rows
            .iter()
            .map(|r| {
                (0..r.columns().len())
                    .map(|i| raw_cell(r, i))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((cols, out))
    })
}

impl AstAdapter for MysqlAdapter {
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
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.model.default_value(node)
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
