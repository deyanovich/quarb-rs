//! Oracle Database adapter for the Quarb query engine.
//!
//! A catalog driver over the shared relational model
//! (`quarb-relational`): it connects with the `oracle` crate
//! (ODPI-C underneath — synchronous, so no internal runtime; the
//! Oracle client library loads dynamically at connect),
//! introspects the connected user's schema through the `USER_*`
//! data-dictionary views — tables, columns with types and
//! scales, primary keys, and foreign keys (`user_cons_columns`
//! joined position-to-position, so composite keys stay paired) —
//! and supplies rows lazily, per table on first touch.
//!
//! Type mapping: `NUMBER(p,0)` arrives as integers,
//! `BINARY_FLOAT`/`BINARY_DOUBLE` as floats, the character
//! family (`VARCHAR2`, `NVARCHAR2`, `CHAR`, `NCHAR`, `CLOB`) as
//! text; `DATE` and `TIMESTAMP` columns are selected with an
//! ISO-8601 `TO_CHAR`, where Quarb's temporal reading takes
//! over; unscaled `NUMBER` and everything else go through a
//! plain `TO_CHAR`, the numeric-reading posture of the other
//! drivers. `LONG`, `BLOB`, and friends select as SQL `NULL`
//! rather than break the table. SQL `NULL` is null.
//!
//! The arbor mapping and the foreign-key reference machinery
//! (`~>` chains, `->`/`<-` crosslinks, `<~` reverse resolution)
//! are documented on the shared model. Read-only: the adapter
//! only ever issues SELECTs.
//!
//! **Target**: `oracle://USER:PASS@HOST[:PORT]/SERVICE` (port
//! defaults to 1521), or any Easy Connect / TNS string via
//! `USER:PASS@CONNECT_STRING`.

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use std::cell::RefCell;
use std::rc::Rc;

/// An error connecting to or loading a database.
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    #[error("oracle: {0}")]
    Oracle(#[from] oracle::Error),
    #[error("oracle target: {0} (expected oracle://USER:PASS@HOST[:PORT]/SERVICE)")]
    Target(String),
    #[error("pushdown plan: {0}")]
    Plan(String),
}

/// How a column decodes, from its dictionary type and scale.
#[derive(Clone, Copy, PartialEq)]
enum Decode {
    Int,
    Float,
    Text,
    /// DATE / TIMESTAMP, selected with an ISO-8601 TO_CHAR.
    DateIso,
    /// TIMESTAMP WITH TIME ZONE, ISO with offset.
    DateIsoTz,
    /// Unrepresentable (LONG, BLOB, BFILE, …): selected as NULL.
    Skip,
}

fn decode_of(data_type: &str, scale: Option<i32>) -> Decode {
    match data_type {
        "NUMBER" | "FLOAT" => match scale {
            Some(0) => Decode::Int,
            // Unspecified or fractional scale: TO_CHAR, and
            // Quarb's numeric reading takes over.
            _ => Decode::Text,
        },
        "BINARY_FLOAT" | "BINARY_DOUBLE" => Decode::Float,
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" | "CLOB" | "NCLOB" => Decode::Text,
        "DATE" => Decode::DateIso,
        t if t.starts_with("TIMESTAMP") && t.contains("TIME ZONE") => Decode::DateIsoTz,
        t if t.starts_with("TIMESTAMP") => Decode::DateIso,
        "RAW" | "INTERVAL" => Decode::Text,
        t if t.starts_with("INTERVAL") => Decode::Text,
        "LONG" | "LONG RAW" | "BLOB" | "BFILE" => Decode::Skip,
        _ => Decode::Text,
    }
}

/// Quote an identifier: double quotes, `"` doubled. Dictionary
/// names come back uppercase and quoting preserves them exactly.
fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// The select expression for a column under its decode.
fn select_expr(col: &str, d: Decode) -> String {
    let q = quote(col);
    match d {
        Decode::Int | Decode::Float => q.clone(),
        Decode::Text => format!("TO_CHAR({q}) AS {q}"),
        Decode::DateIso => {
            format!("TO_CHAR({q}, 'YYYY-MM-DD\"T\"HH24:MI:SS.FF3') AS {q}")
        }
        Decode::DateIsoTz => {
            format!("TO_CHAR({q}, 'YYYY-MM-DD\"T\"HH24:MI:SS.FF3TZH:TZM') AS {q}")
        }
        Decode::Skip => format!("NULL AS {q}"),
    }
}

fn cell(row: &oracle::Row, i: usize, d: Decode) -> Value {
    match d {
        Decode::Int => row
            .get::<usize, Option<i64>>(i)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Int),
        Decode::Float => row
            .get::<usize, Option<f64>>(i)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Float),
        Decode::Skip => Value::Null,
        _ => row
            .get::<usize, Option<String>>(i)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::Str),
    }
}

/// A `DATE` TO_CHAR without fractional seconds (plain DATE has
/// none; asking for `.FF3` on DATE errors).
fn date_expr(col: &str) -> String {
    let q = quote(col);
    format!("TO_CHAR({q}, 'YYYY-MM-DD\"T\"HH24:MI:SS') AS {q}")
}

/// Parse the target into (user, pass, connect string).
fn parse_target(target: &str) -> Result<(String, String, String), OracleError> {
    let rest = target
        .strip_prefix("oracle://")
        .ok_or_else(|| OracleError::Target(target.to_string()))?;
    let (auth, hostpart) = rest
        .split_once('@')
        .ok_or_else(|| OracleError::Target(target.to_string()))?;
    let (user, pass) = auth
        .split_once(':')
        .ok_or_else(|| OracleError::Target(target.to_string()))?;
    // A host/service pair becomes an Easy Connect string; any
    // other shape (already //…, or a TNS alias) passes through.
    let connect = if let Some((host, service)) = hostpart.split_once('/') {
        if hostpart.starts_with("//") {
            hostpart.to_string()
        } else {
            format!("//{host}/{service}")
        }
    } else {
        hostpart.to_string()
    };
    Ok((user.to_string(), pass.to_string(), connect))
}

/// An Oracle schema, exposed as an arbor.
pub struct OracleAdapter {
    model: RelationalModel,
}

impl OracleAdapter {
    /// Connect and introspect the connected user's tables. Rows
    /// load lazily, per table on first touch.
    pub fn connect(target: &str) -> Result<Self, OracleError> {
        Self::connect_impl(target, None)
    }

    /// [`connect`], with one table's fetch filtered by a WHERE
    /// clause (partial pushdown; the engine re-applies the
    /// predicates).
    pub fn connect_filtered(
        target: &str,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, OracleError> {
        Self::connect_impl(target, Some((table.to_string(), where_sql.to_string())))
    }

    fn connect_impl(
        target: &str,
        filter: Option<(String, String)>,
    ) -> Result<Self, OracleError> {
        let (user, pass, connect) = parse_target(target)?;
        let conn = oracle::Connection::connect(&user, &pass, &connect)?;
        let (specs, decodes) = introspect(&conn)?;
        let conn = Rc::new(RefCell::new(conn));
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |t, spec| {
                let conn = conn.borrow_mut();
                let w = filter
                    .as_ref()
                    .filter(|(tn, _)| *tn == spec.name)
                    .map(|(_, w)| w.as_str());
                fetch_rows(&conn, spec, &decodes[t], w).map_err(|e| e.to_string())
            }),
        );
        Ok(OracleAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// The catalog: every table the connected user owns, plus each
/// column's decode kind (DATE tracked apart from TIMESTAMP so
/// its TO_CHAR omits fractional seconds).
#[allow(clippy::type_complexity)]
fn introspect(
    conn: &oracle::Connection,
) -> Result<(Vec<TableSpec>, Vec<Vec<(Decode, bool)>>), OracleError> {
    let mut names = Vec::new();
    for r in conn.query("SELECT table_name FROM user_tables ORDER BY table_name", &[])? {
        names.push(r?.get::<usize, String>(0)?);
    }

    let mut specs = Vec::new();
    let mut all_decodes = Vec::new();
    for name in names {
        let mut columns = Vec::new();
        let mut decodes = Vec::new();
        for r in conn.query(
            "SELECT column_name, data_type, data_scale \
             FROM user_tab_columns WHERE table_name = :1 \
             ORDER BY column_id",
            &[&name],
        )? {
            let r = r?;
            let col: String = r.get(0)?;
            let dt: String = r.get(1)?;
            let scale: Option<i32> = r.get(2)?;
            columns.push(col);
            decodes.push((decode_of(&dt, scale), dt == "DATE"));
        }

        let mut pk_cols = Vec::new();
        for r in conn.query(
            "SELECT cols.column_name \
             FROM user_constraints cons \
             JOIN user_cons_columns cols \
               ON cons.constraint_name = cols.constraint_name \
             WHERE cons.constraint_type = 'P' AND cons.table_name = :1 \
             ORDER BY cols.position",
            &[&name],
        )? {
            pk_cols.push(r?.get::<usize, String>(0)?);
        }
        let pk = (pk_cols.len() == 1)
            .then(|| columns.iter().position(|c| *c == pk_cols[0]))
            .flatten();

        let mut fks = Vec::new();
        for r in conn.query(
            "SELECT a.column_name, c_pk.table_name, b.column_name \
             FROM user_constraints c \
             JOIN user_cons_columns a \
               ON c.constraint_name = a.constraint_name \
             JOIN user_constraints c_pk \
               ON c.r_constraint_name = c_pk.constraint_name \
             JOIN user_cons_columns b \
               ON c_pk.constraint_name = b.constraint_name \
              AND b.position = a.position \
             WHERE c.constraint_type = 'R' AND c.table_name = :1 \
             ORDER BY c.constraint_name, a.position",
            &[&name],
        )? {
            let r = r?;
            let from: String = r.get(0)?;
            let target: String = r.get(1)?;
            let to: String = r.get(2)?;
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

/// Stream one table's rows; row order follows the key when one
/// names rows.
fn fetch_rows(
    conn: &oracle::Connection,
    spec: &TableSpec,
    decodes: &[(Decode, bool)],
    where_sql: Option<&str>,
) -> Result<Vec<RowSpec>, OracleError> {
    let select: Vec<String> = spec
        .columns
        .iter()
        .zip(decodes)
        .map(|(c, (d, is_date))| match (d, is_date) {
            (Decode::DateIso, true) => date_expr(c),
            _ => select_expr(c, *d),
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
    let sql = format!(
        "SELECT {} FROM {}{filter}{order}",
        select.join(", "),
        quote(&spec.name)
    );
    let mut out = Vec::new();
    for (i, r) in conn.query(&sql, &[])?.enumerate() {
        let r = r?;
        out.push(RowSpec {
            rowid: i as i64 + 1,
            values: (0..spec.columns.len())
                .map(|c| cell(&r, c, decodes[c].0))
                .collect(),
        });
    }
    Ok(out)
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s key when one is given.
pub fn raw_query(
    target: &str,
    sql: &str,
    order_table: Option<&str>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), OracleError> {
    if join_left.is_some() {
        return Err(OracleError::Plan(
            "witness-JOIN uniqueness not verified by this driver".into(),
        ));
    }
    let (user, pass, connect) = parse_target(target)?;
    let conn = oracle::Connection::connect(&user, &pass, &connect)?;
    let sql = match order_table {
        Some(t) => {
            let (specs, _) = introspect(&conn)?;
            let key = specs
                .into_iter()
                .find(|s| s.name == t)
                .and_then(|s| s.pk.map(|i| s.columns[i].clone()));
            match key {
                Some(k) => format!("{sql} ORDER BY {}.{}", quote(t), quote(&k)),
                None => sql.to_string(),
            }
        }
        None => sql.to_string(),
    };
    let rows = conn.query(&sql, &[])?;
    let cols: Vec<String> = rows
        .column_info()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    let n = cols.len();
    let mut out = Vec::new();
    for r in rows {
        let r = r?;
        let mut vals = Vec::with_capacity(n);
        for i in 0..n {
            let v = if let Ok(Some(x)) = r.get::<usize, Option<i64>>(i) {
                Value::Int(x)
            } else if let Ok(Some(x)) = r.get::<usize, Option<f64>>(i) {
                Value::Float(x)
            } else if let Ok(x) = r.get::<usize, Option<String>>(i) {
                x.map_or(Value::Null, Value::Str)
            } else {
                return Err(OracleError::Plan("undecodable column".into()));
            };
            vals.push(v);
        }
        out.push(vals);
    }
    Ok((cols, out))
}

impl AstAdapter for OracleAdapter {
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
        let (u, p, c) = parse_target("oracle://music:pw@localhost:1521/FREEPDB1").unwrap();
        assert_eq!((u.as_str(), p.as_str()), ("music", "pw"));
        assert_eq!(c, "//localhost:1521/FREEPDB1");
        let (_, _, c) = parse_target("oracle://u:p@TNSALIAS").unwrap();
        assert_eq!(c, "TNSALIAS");
        assert!(parse_target("mssql://x").is_err());
    }

    #[test]
    fn number_scale_decides() {
        assert!(matches!(decode_of("NUMBER", Some(0)), Decode::Int));
        assert!(matches!(decode_of("NUMBER", Some(2)), Decode::Text));
        assert!(matches!(decode_of("NUMBER", None), Decode::Text));
        assert!(matches!(decode_of("BINARY_DOUBLE", None), Decode::Float));
        assert!(matches!(decode_of("DATE", None), Decode::DateIso));
        assert!(matches!(
            decode_of("TIMESTAMP(6) WITH TIME ZONE", None),
            Decode::DateIsoTz
        ));
        assert!(matches!(decode_of("BLOB", None), Decode::Skip));
    }
}
