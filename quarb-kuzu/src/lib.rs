//! Kùzu embedded-graph adapter for the Quarb query engine.
//!
//! Kùzu is to graphs what DuckDB is to tables: an in-process,
//! file-based engine — no server, no credentials, a directory on
//! disk. Its schema-full property graph maps onto the arbor the
//! same way Neo4j's does, with one pleasant upgrade: **node
//! tables declare real primary keys**, so rows are named by their
//! primary-key value with no `?key=` nomination needed.
//!
//! The root holds one child per **node table**; a table holds its
//! rows (ordered by primary key — deterministic listings); and
//! **rel tables become labeled crosslinks**: `->LIVES_IN` follows
//! outgoing rels of that type, `<-LIVES_IN` incoming, `->*` any.
//! Rel properties answer the `$-` edge accessor
//! (`->FOLLOWS[$-::since > 2020]`). Typed columns keep their
//! types: TIMESTAMP/DATE mint instants, INTERVAL mints a
//! duration, lists stay lists.
//!
//! The database opens **read-only** — the engine's doctrine, here
//! enforced by Kùzu itself.
//!
//! **Target**: `kuzu:PATH` — the database directory.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// An error opening or reading a Kùzu database.
#[derive(Debug, thiserror::Error)]
pub enum KuzuError {
    #[error("kuzu: {0}")]
    Api(String),
    #[error("kuzu target: {0} (expected kuzu:PATH)")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> KuzuError {
    KuzuError::Api(e.to_string())
}

/// A table name in backticks, backticks doubled.
fn bt(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// A Kùzu value as a Quarb value. Dates and timestamps mint
/// instants, intervals mint durations; structs, maps, and blobs
/// take the text posture.
fn cell_value(v: &kuzu::Value) -> Value {
    use kuzu::Value as K;
    match v {
        K::Null(_) => Value::Null,
        K::Bool(b) => Value::Bool(*b),
        K::Int64(n) => Value::Int(*n),
        K::Int32(n) => Value::Int(*n as i64),
        K::Int16(n) => Value::Int(*n as i64),
        K::Int8(n) => Value::Int(*n as i64),
        K::UInt64(n) => i64::try_from(*n)
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(n.to_string())),
        K::UInt32(n) => Value::Int(*n as i64),
        K::UInt16(n) => Value::Int(*n as i64),
        K::UInt8(n) => Value::Int(*n as i64),
        K::Int128(n) => i64::try_from(*n)
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(n.to_string())),
        K::Double(f) => Value::Float(*f),
        K::Float(f) => Value::Float(*f as f64),
        K::String(s) => Value::Str(s.clone()),
        K::Date(d) => Value::Instant {
            secs: (d.to_julian_day() as i64 - 2_440_588) * 86_400,
            nanos: 0,
            offset_min: None,
        },
        K::Timestamp(t)
        | K::TimestampTz(t)
        | K::TimestampNs(t)
        | K::TimestampMs(t)
        | K::TimestampSec(t) => Value::Instant {
            secs: t.unix_timestamp(),
            nanos: t.nanosecond(),
            offset_min: None,
        },
        K::Interval(d) => {
            let mut secs = d.whole_seconds();
            let mut nanos = d.subsec_nanoseconds();
            if nanos < 0 {
                secs -= 1;
                nanos += 1_000_000_000;
            }
            Value::Duration {
                secs,
                nanos: nanos as u32,
            }
        }
        K::List(_, items) | K::Array(_, items) => {
            Value::List(items.iter().map(cell_value).collect())
        }
        other => Value::Str(other.to_string()),
    }
}

/// A Quarb value as a Kùzu parameter, type-preserving — a
/// primary-key lookup against an INT64 key must send an integer.
fn value_to_kuzu(v: &Value) -> kuzu::Value {
    match v {
        Value::Bool(b) => kuzu::Value::Bool(*b),
        Value::Int(n) => kuzu::Value::Int64(*n),
        Value::Float(f) => kuzu::Value::Double(*f),
        other => kuzu::Value::String(other.to_string()),
    }
}

/// One rel table's catalog entry: name, source and destination
/// node tables (the connectivity is catalog fact — surfaced as
/// root metadata, not needed for matching, which goes untyped).
struct RelTable {
    name: String,
    src: String,
    dst: String,
}

enum Kind {
    Root,
    Table {
        index: usize,
    },
    /// A row: its table, its primary-key value (for parameterized
    /// re-finds), and its decoded properties.
    Entity {
        table: String,
        pk: Value,
        props: Vec<(String, Value)>,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A Kùzu database, exposed as an arbor.
pub struct KuzuAdapter {
    db: kuzu::Database,
    /// Node tables, sorted; table i is node i + 1.
    tables: Vec<String>,
    /// Each node table's primary-key column.
    pks: HashMap<String, String>,
    rels: Vec<RelTable>,
    nodes: RefCell<Vec<Node>>,
    /// (table_id, offset) → interned entity node.
    by_iid: RefCell<HashMap<(u64, u64), NodeId>>,
    /// Rel properties, cached as edges are fetched.
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl KuzuAdapter {
    /// Open `kuzu:PATH` read-only; the catalog scan doubles as the
    /// open probe.
    pub fn open(target: &str) -> Result<Self, KuzuError> {
        let path = target
            .strip_prefix("kuzu:")
            .ok_or_else(|| KuzuError::Target(target.to_string()))?;
        if path.is_empty() {
            return Err(KuzuError::Target(target.to_string()));
        }
        let db = kuzu::Database::new(path, kuzu::SystemConfig::default().read_only(true))
            .map_err(api)?;
        let mut tables = Vec::new();
        let mut rel_names = Vec::new();
        {
            let conn = kuzu::Connection::new(&db).map_err(api)?;
            let result = conn.query("CALL show_tables() RETURN *").map_err(api)?;
            let cols = result.get_column_names();
            let idx = |want: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(want));
            let (ni, ti) = (idx("name"), idx("type"));
            for row in result {
                let (Some(ni), Some(ti)) = (ni, ti) else { break };
                let name = match row.get(ni) {
                    Some(kuzu::Value::String(s)) => s.clone(),
                    _ => continue,
                };
                match row.get(ti) {
                    Some(kuzu::Value::String(t)) if t == "NODE" => tables.push(name),
                    Some(kuzu::Value::String(t)) if t == "REL" => rel_names.push(name),
                    _ => {}
                }
            }
            tables.sort();
            rel_names.sort();
        }
        let mut pks = HashMap::new();
        let mut rels = Vec::new();
        {
            let conn = kuzu::Connection::new(&db).map_err(api)?;
            for t in &tables {
                let result = conn
                    .query(&format!(
                        "CALL table_info('{}') RETURN *",
                        t.replace('\'', "''")
                    ))
                    .map_err(api)?;
                let cols = result.get_column_names();
                let name_i = cols.iter().position(|c| c.eq_ignore_ascii_case("name"));
                let pk_i = cols
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case("primary key"));
                for row in result {
                    if let (Some(ni), Some(pi)) = (name_i, pk_i)
                        && let (Some(kuzu::Value::String(n)), Some(kuzu::Value::Bool(true))) =
                            (row.get(ni), row.get(pi))
                    {
                        pks.insert(t.clone(), n.clone());
                    }
                }
            }
            for r in &rel_names {
                let result = conn
                    .query(&format!(
                        "CALL show_connection('{}') RETURN *",
                        r.replace('\'', "''")
                    ))
                    .map_err(api)?;
                for row in result {
                    if let (Some(kuzu::Value::String(src)), Some(kuzu::Value::String(dst))) =
                        (row.first(), row.get(1))
                    {
                        rels.push(RelTable {
                            name: r.clone(),
                            src: src.clone(),
                            dst: dst.clone(),
                        });
                    }
                }
            }
        }
        let mut nodes = vec![Node {
            kind: Kind::Root,
            name: None,
            parent: None,
            children: RefCell::new(None),
        }];
        for (i, t) in tables.iter().enumerate() {
            nodes.push(Node {
                kind: Kind::Table { index: i },
                name: Some(t.clone()),
                parent: Some(NodeId(0)),
                children: RefCell::new(None),
            });
        }
        Ok(KuzuAdapter {
            db,
            tables,
            pks,
            rels,
            nodes: RefCell::new(nodes),
            by_iid: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        })
    }

    /// A human-readable locator: `/Table/pk-value`.
    pub fn locator(&self, node: NodeId) -> String {
        let nodes = self.nodes.borrow();
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            if let Some(name) = &nodes[n.0 as usize].name {
                parts.push(name.clone());
            }
            cur = nodes[n.0 as usize].parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// Intern one fetched NodeVal, reusing a previous mint.
    fn intern(&self, nv: &kuzu::NodeVal) -> Option<NodeId> {
        let iid = nv.get_node_id();
        let key = (iid.table_id, iid.offset);
        if let Some(&id) = self.by_iid.borrow().get(&key) {
            return Some(id);
        }
        let table = nv.get_label_name().clone();
        let props: Vec<(String, Value)> = nv
            .get_properties()
            .iter()
            .map(|(k, v)| (k.clone(), cell_value(v)))
            .collect();
        let pk_col = self.pks.get(&table)?;
        let pk = props
            .iter()
            .find(|(k, _)| k == pk_col)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Null);
        let parent = self
            .tables
            .iter()
            .position(|t| t == &table)
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            name: Some(pk.to_string()),
            kind: Kind::Entity { table, pk, props },
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_iid.borrow_mut().insert(key, id);
        Some(id)
    }

    /// Run a statement with a `$v` parameter bound to a
    /// primary-key value.
    fn query_v(&self, stmt: &str, v: &Value) -> Result<Vec<Vec<kuzu::Value>>, KuzuError> {
        let conn = kuzu::Connection::new(&self.db).map_err(api)?;
        let mut prepared = conn.prepare(stmt).map_err(api)?;
        let result = conn
            .execute(&mut prepared, vec![("v", value_to_kuzu(v))])
            .map_err(api)?;
        Ok(result.collect())
    }

    /// One node's rels, outgoing or incoming, as
    /// `(type, interned other end)` pairs — a single untyped MATCH
    /// per direction.
    fn edges(&self, node: NodeId, incoming: bool) -> Vec<(String, NodeId)> {
        let (table, pk) = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { table, pk, .. } => (table.clone(), pk.clone()),
            _ => return Vec::new(),
        };
        let Some(pk_col) = self.pks.get(&table).cloned() else {
            return Vec::new();
        };
        let arrow = if incoming { "<-[r]-" } else { "-[r]->" };
        let stmt = format!(
            "MATCH (n:{}){arrow}(m) WHERE n.{} = $v RETURN r, m",
            bt(&table),
            bt(&pk_col)
        );
        let rows = self.query_v(&stmt, &pk).unwrap_or_default();
        let mut out: Vec<(String, NodeId)> = rows
            .iter()
            .filter_map(|row| {
                let (kuzu::Value::Rel(rv), kuzu::Value::Node(nv)) = (row.first()?, row.get(1)?)
                else {
                    return None;
                };
                let other = self.intern(nv)?;
                let label = rv.get_label_name().clone();
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                let props: Vec<(String, Value)> = rv
                    .get_properties()
                    .iter()
                    .map(|(k, v)| (k.clone(), cell_value(v)))
                    .collect();
                self.edge_props
                    .borrow_mut()
                    .entry((source, label.clone(), target))
                    .or_insert(props);
                Some((label, other))
            })
            .collect();
        // Deterministic edge order: by type, then target name.
        out.sort_by(|a, b| {
            let nodes = self.nodes.borrow();
            (&a.0, &nodes[a.1.0 as usize].name).cmp(&(&b.0, &nodes[b.1.0 as usize].name))
        });
        out
    }
}

impl AstAdapter for KuzuAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if let Some(c) = self.nodes.borrow()[node.0 as usize]
            .children
            .borrow()
            .as_ref()
        {
            return c.clone();
        }
        let table = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => {
                return (1..=self.tables.len()).map(|i| NodeId(i as u64)).collect();
            }
            Kind::Entity { .. } => return Vec::new(),
            Kind::Table { index } => self.tables[*index].clone(),
        };
        let pk = self.pks.get(&table).cloned().unwrap_or_default();
        let stmt = format!("MATCH (m:{}) RETURN m ORDER BY m.{}", bt(&table), bt(&pk));
        let rows: Vec<Vec<kuzu::Value>> = match kuzu::Connection::new(&self.db)
            .and_then(|c| c.query(&stmt).map(|r| r.collect()))
        {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let ids: Vec<NodeId> = rows
            .iter()
            .filter_map(|row| match row.first() {
                Some(kuzu::Value::Node(nv)) => self.intern(nv),
                _ => None,
            })
            .collect();
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Vec::new(),
            Kind::Table { .. } => vec!["table".to_string()],
            Kind::Entity { table, .. } => vec![table.clone()],
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { props, .. } => props
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .filter(|v| !matches!(v, Value::Null)),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let nodes = self.nodes.borrow();
        match (&nodes[node.0 as usize].kind, key) {
            (Kind::Root, "tables") => Some(Value::List(
                self.tables.iter().map(|t| Value::Str(t.clone())).collect(),
            )),
            (Kind::Root, "rel-types") => {
                let mut names: Vec<String> = self.rels.iter().map(|r| r.name.clone()).collect();
                names.dedup();
                Some(Value::List(names.into_iter().map(Value::Str).collect()))
            }
            (Kind::Root, "rel-connections") => Some(Value::List(
                self.rels
                    .iter()
                    .map(|r| Value::Str(format!("{}: {} -> {}", r.name, r.src, r.dst)))
                    .collect(),
            )),
            (Kind::Table { index }, "primary-key") => {
                self.pks.get(&self.tables[*index]).cloned().map(Value::Str)
            }
            (Kind::Table { index }, "n-rows") => {
                let stmt = format!("MATCH (m:{}) RETURN count(m)", bt(&self.tables[*index]));
                drop(nodes);
                let conn = kuzu::Connection::new(&self.db).ok()?;
                let mut result = conn.query(&stmt).ok()?;
                result.next()?.first().map(cell_value)
            }
            (Kind::Entity { table, .. }, "table") => Some(Value::Str(table.clone())),
            _ => None,
        }
    }

    /// Outgoing rels as `(type, target)` crosslinks.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, false)
    }

    /// Incoming rels as `(type, source)` crosslinks.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, true)
    }

    /// Hint-form resolution: `::city~>City` matches the property's
    /// value against the target table's primary key.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let table = hint?;
        let value = self.property(node, property)?;
        let pk = self.pks.get(table)?.clone();
        let stmt = format!(
            "MATCH (m:{}) WHERE m.{} = $v RETURN m LIMIT 1",
            bt(table),
            bt(&pk)
        );
        let rows = self.query_v(&stmt, &value).ok()?;
        match rows.first()?.first()? {
            kuzu::Value::Node(nv) => self.intern(nv),
            _ => None,
        }
    }

    /// `$-::prop` — a rel's own property, from the cache the edge
    /// fetch filled; a cold read refetches the source's edges.
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        if let Some(props) = self
            .edge_props
            .borrow()
            .get(&(source, label.to_string(), target))
        {
            return props.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        }
        self.edges(source, false);
        self.edge_props
            .borrow()
            .get(&(source, label.to_string(), target))?
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_convert() {
        assert_eq!(cell_value(&kuzu::Value::Int64(42)), Value::Int(42));
        assert_eq!(cell_value(&kuzu::Value::Double(1.5)), Value::Float(1.5));
        assert_eq!(cell_value(&kuzu::Value::Bool(true)), Value::Bool(true));
        assert_eq!(
            cell_value(&kuzu::Value::String("x".into())),
            Value::Str("x".to_string())
        );
        // 1970-01-01 is instant zero, and dates print bare.
        let d = kuzu::Value::Date(
            time::Date::from_calendar_date(1970, time::Month::January, 1).unwrap(),
        );
        assert_eq!(
            cell_value(&d),
            Value::Instant {
                secs: 0,
                nanos: 0,
                offset_min: None
            }
        );
    }

    #[test]
    fn param_types_preserved() {
        assert!(matches!(
            value_to_kuzu(&Value::Int(7)),
            kuzu::Value::Int64(7)
        ));
        assert!(matches!(
            value_to_kuzu(&Value::Str("ada".into())),
            kuzu::Value::String(s) if s == "ada"
        ));
    }

    #[test]
    fn quoting() {
        assert_eq!(bt("City"), "`City`");
        assert_eq!(bt("od`d"), "`od``d`");
    }
}
