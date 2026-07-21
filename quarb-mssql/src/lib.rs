//! Microsoft SQL Server adapter for the Quarb query engine.
//!
//! A catalog driver over the shared relational model
//! (`quarb-relational`): it connects with `tiberius` (on an
//! internal current-thread runtime, so the public surface stays
//! synchronous), introspects the `dbo` schema —
//! `INFORMATION_SCHEMA` for tables, columns, and the primary
//! key; `sys.foreign_key_columns` for foreign keys, whose
//! parallel column ids keep composite keys paired — and supplies
//! rows lazily, per table on first touch.
//!
//! Type mapping: `bit` arrives as boolean, the integer family as
//! integers, `real`/`float` as floats, the character family as
//! text; every other type (decimal, dates, uniqueidentifier,
//! xml, …) is selected with a `CAST(... AS NVARCHAR(MAX))`,
//! where Quarb's numeric reading and comparisons take over — the
//! same posture as the other relational drivers. SQL `NULL` is
//! null.
//!
//! The arbor mapping and the foreign-key reference machinery
//! (`~>` chains, `->`/`<-` crosslinks, `<~` reverse resolution)
//! are documented on the shared model. Read-only: the adapter
//! only ever issues SELECTs.
//!
//! **Target**: `mssql://USER:PASS@HOST[:PORT]/DATABASE` (port
//! defaults to 1433), or an ADO connection string (anything
//! containing `=`). The TLS certificate is trusted as presented
//! — the posture of a development/analysis connection, not a
//! hardened deployment.

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use std::cell::RefCell;
use std::rc::Rc;
use tiberius::{AuthMethod, Client, Config, Row};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type Conn = Client<Compat<tokio::net::TcpStream>>;

/// An error connecting to or loading a database.
#[derive(Debug, thiserror::Error)]
pub enum MssqlError {
    #[error("mssql: {0}")]
    Mssql(#[from] tiberius::error::Error),
    #[error("mssql target: {0} (expected mssql://USER:PASS@HOST[:PORT]/DATABASE)")]
    Target(String),
    #[error("pushdown plan: {0}")]
    Plan(String),
    #[error("mssql runtime: {0}")]
    Runtime(#[from] std::io::Error),
}

/// How a column decodes, from its `INFORMATION_SCHEMA` type.
#[derive(Clone, Copy, PartialEq)]
enum Decode {
    Bool,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Real,
    Float,
    /// Native character data, and every cast column.
    Text,
}

fn native_decode(data_type: &str) -> Option<Decode> {
    Some(match data_type {
        "bit" => Decode::Bool,
        "tinyint" => Decode::TinyInt,
        "smallint" => Decode::SmallInt,
        "int" => Decode::Int,
        "bigint" => Decode::BigInt,
        "real" => Decode::Real,
        "float" => Decode::Float,
        "char" | "varchar" | "text" | "nchar" | "nvarchar" | "ntext" => Decode::Text,
        _ => return None,
    })
}

/// Quote an identifier T-SQL style: `[name]`, `]` doubled.
fn quote(ident: &str) -> String {
    format!("[{}]", ident.replace(']', "]]"))
}

fn cell(row: &Row, i: usize, d: Decode) -> Value {
    match d {
        Decode::Bool => row
            .get::<bool, _>(i)
            .map_or(Value::Null, Value::Bool),
        Decode::TinyInt => row
            .get::<u8, _>(i)
            .map_or(Value::Null, |n| Value::Int(n as i64)),
        Decode::SmallInt => row
            .get::<i16, _>(i)
            .map_or(Value::Null, |n| Value::Int(n as i64)),
        Decode::Int => row
            .get::<i32, _>(i)
            .map_or(Value::Null, |n| Value::Int(n as i64)),
        Decode::BigInt => row.get::<i64, _>(i).map_or(Value::Null, Value::Int),
        Decode::Real => row
            .get::<f32, _>(i)
            .map_or(Value::Null, |f| Value::Float(f as f64)),
        Decode::Float => row.get::<f64, _>(i).map_or(Value::Null, Value::Float),
        Decode::Text => row
            .get::<&str, _>(i)
            .map_or(Value::Null, |s| Value::Str(s.to_string())),
    }
}

/// A raw (pushdown) cell: try each native shape, then text; an
/// undecodable type errors so the caller falls back to the scan.
fn raw_cell(row: &Row, i: usize) -> Result<Value, MssqlError> {
    if let Ok(Some(b)) = row.try_get::<bool, _>(i) {
        return Ok(Value::Bool(b));
    }
    if let Ok(v) = row.try_get::<u8, _>(i) {
        return Ok(v.map_or(Value::Null, |n| Value::Int(n as i64)));
    }
    if let Ok(v) = row.try_get::<i16, _>(i) {
        return Ok(v.map_or(Value::Null, |n| Value::Int(n as i64)));
    }
    if let Ok(v) = row.try_get::<i32, _>(i) {
        return Ok(v.map_or(Value::Null, |n| Value::Int(n as i64)));
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return Ok(v.map_or(Value::Null, Value::Int));
    }
    if let Ok(v) = row.try_get::<f32, _>(i) {
        return Ok(v.map_or(Value::Null, |f| Value::Float(f as f64)));
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return Ok(v.map_or(Value::Null, Value::Float));
    }
    match row.try_get::<&str, _>(i) {
        Ok(v) => Ok(v.map_or(Value::Null, |s| Value::Str(s.to_string()))),
        Err(e) => Err(MssqlError::Plan(format!("undecodable column: {e}"))),
    }
}

/// Parse the target into a tiberius `Config`.
fn parse_target(target: &str) -> Result<Config, MssqlError> {
    if let Some(rest) = target.strip_prefix("mssql://") {
        let (auth, hostpart) = rest
            .split_once('@')
            .ok_or_else(|| MssqlError::Target(target.to_string()))?;
        let (user, pass) = auth
            .split_once(':')
            .ok_or_else(|| MssqlError::Target(target.to_string()))?;
        let (host, db) = hostpart
            .split_once('/')
            .ok_or_else(|| MssqlError::Target(target.to_string()))?;
        let (host, port) = match host.split_once(':') {
            Some((h, p)) => (
                h,
                p.parse::<u16>()
                    .map_err(|_| MssqlError::Target(target.to_string()))?,
            ),
            None => (host, 1433),
        };
        let mut config = Config::new();
        config.host(host);
        config.port(port);
        config.database(db);
        config.authentication(AuthMethod::sql_server(user, pass));
        config.trust_cert();
        Ok(config)
    } else if target.contains('=') {
        let mut config =
            Config::from_ado_string(target).map_err(MssqlError::Mssql)?;
        config.trust_cert();
        Ok(config)
    } else {
        Err(MssqlError::Target(target.to_string()))
    }
}

async fn open(config: Config) -> Result<Conn, MssqlError> {
    let tcp = tokio::net::TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Ok(Client::connect(config, tcp.compat_write()).await?)
}

/// A SQL Server database, exposed as an arbor.
pub struct MssqlAdapter {
    model: RelationalModel,
}

impl MssqlAdapter {
    /// Connect and introspect the `dbo` schema. Rows load
    /// lazily, per table on first touch.
    pub fn connect(target: &str) -> Result<Self, MssqlError> {
        Self::connect_impl(target, None)
    }

    /// [`connect`], with one table's fetch filtered by a WHERE
    /// clause (partial pushdown; the engine re-applies the
    /// predicates).
    pub fn connect_filtered(
        target: &str,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, MssqlError> {
        Self::connect_impl(target, Some((table.to_string(), where_sql.to_string())))
    }

    fn connect_impl(
        target: &str,
        filter: Option<(String, String)>,
    ) -> Result<Self, MssqlError> {
        let config = parse_target(target)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (conn, specs, decodes) = rt.block_on(async {
            let mut conn = open(config).await?;
            let (specs, decodes) = introspect(&mut conn).await?;
            Ok::<_, MssqlError>((conn, specs, decodes))
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
        Ok(MssqlAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// The catalog: every `dbo` table's spec plus each column's
/// decode kind.
async fn introspect(
    conn: &mut Conn,
) -> Result<(Vec<TableSpec>, Vec<Vec<Decode>>), MssqlError> {
    let names: Vec<String> = conn
        .query(
            "SELECT table_name FROM INFORMATION_SCHEMA.TABLES \
             WHERE table_schema = 'dbo' AND table_type = 'BASE TABLE' \
             ORDER BY table_name",
            &[],
        )
        .await?
        .into_first_result()
        .await?
        .iter()
        .filter_map(|r| r.get::<&str, _>(0).map(str::to_string))
        .collect();

    let mut specs = Vec::new();
    let mut all_decodes = Vec::new();
    for name in names {
        let cols = conn
            .query(
                "SELECT column_name, data_type \
                 FROM INFORMATION_SCHEMA.COLUMNS \
                 WHERE table_schema = 'dbo' AND table_name = @P1 \
                 ORDER BY ordinal_position",
                &[&name.as_str()],
            )
            .await?
            .into_first_result()
            .await?;
        let columns: Vec<String> = cols
            .iter()
            .filter_map(|r| r.get::<&str, _>(0).map(str::to_string))
            .collect();
        let decodes: Vec<Decode> = cols
            .iter()
            .map(|r| {
                r.get::<&str, _>(1)
                    .and_then(native_decode)
                    .unwrap_or(Decode::Text)
            })
            .collect();

        // The primary key (single-column keys name the rows).
        let pk_rows = conn
            .query(
                "SELECT kcu.column_name \
                 FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
                 JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.table_schema = kcu.table_schema \
                  AND tc.table_name = kcu.table_name \
                 WHERE tc.table_schema = 'dbo' AND tc.table_name = @P1 \
                   AND tc.constraint_type = 'PRIMARY KEY' \
                 ORDER BY kcu.ordinal_position",
                &[&name.as_str()],
            )
            .await?
            .into_first_result()
            .await?;
        let pk = (pk_rows.len() == 1)
            .then(|| {
                let col = pk_rows[0].get::<&str, _>(0)?;
                columns.iter().position(|c| c == col)
            })
            .flatten();

        // Declared foreign keys from sys.foreign_key_columns:
        // parent/referenced column ids are parallel, so composite
        // keys stay paired.
        let fk_rows = conn
            .query(
                "SELECT cp.name, tr.name, cr.name \
                 FROM sys.foreign_key_columns fkc \
                 JOIN sys.tables tp ON tp.object_id = fkc.parent_object_id \
                 JOIN sys.columns cp ON cp.object_id = fkc.parent_object_id \
                  AND cp.column_id = fkc.parent_column_id \
                 JOIN sys.tables tr ON tr.object_id = fkc.referenced_object_id \
                 JOIN sys.columns cr ON cr.object_id = fkc.referenced_object_id \
                  AND cr.column_id = fkc.referenced_column_id \
                 WHERE tp.name = @P1 AND schema_name(tp.schema_id) = 'dbo' \
                 ORDER BY fkc.constraint_object_id, fkc.constraint_column_id",
                &[&name.as_str()],
            )
            .await?
            .into_first_result()
            .await?;
        let mut fks = Vec::new();
        for r in &fk_rows {
            let (Some(from), Some(target), Some(to)) = (
                r.get::<&str, _>(0),
                r.get::<&str, _>(1),
                r.get::<&str, _>(2),
            ) else {
                continue;
            };
            if let Some(idx) = columns.iter().position(|c| c == from) {
                fks.push((idx, target.to_string(), to.to_string()));
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

/// Stream one table's rows: native columns bare, the rest cast
/// to NVARCHAR; row order follows the key when one names rows.
async fn fetch_rows(
    conn: &mut Conn,
    spec: &TableSpec,
    decodes: &[Decode],
    where_sql: Option<&str>,
) -> Result<Vec<RowSpec>, MssqlError> {
    let select: Vec<String> = spec
        .columns
        .iter()
        .zip(decodes)
        .map(|(c, d)| {
            let q = quote(c);
            match d {
                Decode::Text => format!("CAST({q} AS NVARCHAR(MAX)) AS {q}"),
                _ => q,
            }
        })
        .collect();
    let order = match spec.pk {
        Some(i) => format!(" ORDER BY {}", quote(&spec.columns[i])),
        None => String::new(),
    };
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    let rows = conn
        .query(
            format!(
                "SELECT {} FROM [dbo].{}{filter}{order}",
                select.join(", "),
                quote(&spec.name)
            ),
            &[],
        )
        .await?
        .into_first_result()
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
/// ordered by `order_table`'s key when one is given (the
/// pushdown contract: row order must match document order).
pub fn raw_query(
    target: &str,
    sql: &str,
    order_table: Option<&str>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), MssqlError> {
    // Witness-JOIN plans carry a uniqueness obligation this
    // driver does not verify against its catalog; decline so the
    // caller falls back to the (sound) scan.
    if join_left.is_some() {
        return Err(MssqlError::Plan(
            "witness-JOIN uniqueness not verified by this driver".into(),
        ));
    }
    let config = parse_target(target)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut conn = open(config).await?;
        let sql = match order_table {
            Some(t) => {
                let (specs, _) = introspect(&mut conn).await?;
                let key = specs
                    .into_iter()
                    .find(|s| s.name == t)
                    .and_then(|s| s.pk.map(|i| s.columns[i].clone()));
                match key {
                    Some(k) => {
                        format!("{sql} ORDER BY {}.{}", quote(t), quote(&k))
                    }
                    None => sql.to_string(),
                }
            }
            None => sql.to_string(),
        };
        let rows = conn.query(sql, &[]).await?.into_first_result().await?;
        let cols: Vec<String> = rows
            .first()
            .map(|r| {
                r.columns()
                    .iter()
                    .map(|c| c.name().to_string())
                    .collect()
            })
            .unwrap_or_default();
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

impl AstAdapter for MssqlAdapter {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert!(parse_target("mssql://sa:Passw0rd@localhost/music").is_ok());
        assert!(parse_target("mssql://sa:Passw0rd@db:14330/music").is_ok());
        assert!(parse_target("Server=tcp:h,1433;User Id=sa;Password=x;Database=d").is_ok());
        assert!(matches!(
            parse_target("mysql://x"),
            Err(MssqlError::Target(_))
        ));
        assert!(matches!(
            parse_target("mssql://nopass-host/db"),
            Err(MssqlError::Target(_))
        ));
    }

    #[test]
    fn identifiers_quote() {
        assert_eq!(quote("tracks"), "[tracks]");
        assert_eq!(quote("we]ird"), "[we]]ird]");
    }

    #[test]
    fn native_types_map() {
        assert!(matches!(native_decode("bit"), Some(Decode::Bool)));
        assert!(matches!(native_decode("bigint"), Some(Decode::BigInt)));
        assert!(matches!(native_decode("nvarchar"), Some(Decode::Text)));
        assert!(native_decode("decimal").is_none());
        assert!(native_decode("datetime2").is_none());
    }
}
