//! FalkorDB adapter for the Quarb query engine.
//!
//! FalkorDB (RedisGraph's successor) runs a Cypher-dialect
//! property graph over the Redis protocol — `GRAPH.QUERY` in,
//! nested arrays out. The mapping is the Neo4j adapter's: the
//! root holds one child per **label**, a label holds its nodes,
//! and **relationship types become typed crosslinks**
//! (`->KNOWS`, `<-KNOWS`, `->*`), with relationship properties
//! answering the `$-` edge accessor. `?key=PROP` names nodes by
//! a property; without it the internal id names the node.
//!
//! Replies are read in the verbose (default) format — cells are
//! key/value arrays with `properties` lists — and only read
//! statements are ever sent.
//!
//! **Target**: `falkor://[USER:PASS@]HOST[:PORT]/GRAPH[?key=PROP]`
//! (or `falkors://` for TLS) — the connection underneath is a
//! standard Redis client.

use quarb::{AstAdapter, NodeId, Value};
use redis::Value as R;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a FalkorDB graph.
#[derive(Debug, thiserror::Error)]
pub enum FalkorError {
    #[error("falkordb: {0}")]
    Api(String),
    #[error("falkordb target: {0} (expected falkor://[USER:PASS@]HOST[:PORT]/GRAPH[?key=PROP])")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> FalkorError {
    FalkorError::Api(e.to_string())
}

/// The text of a RESP scalar.
fn text(v: &R) -> Option<String> {
    match v {
        R::BulkString(b) => Some(String::from_utf8_lossy(b).into_owned()),
        R::SimpleString(s) => Some(s.clone()),
        R::VerbatimString { text, .. } => Some(text.clone()),
        R::Int(i) => Some(i.to_string()),
        R::Double(d) => Some(d.to_string()),
        _ => None,
    }
}

/// A verbose-reply cell as a Quarb value.
fn cell_value(v: &R) -> Value {
    match v {
        R::Nil => Value::Null,
        R::Int(i) => Value::Int(*i),
        R::Double(d) => Value::Float(*d),
        R::Boolean(b) => Value::Bool(*b),
        R::BulkString(b) => {
            let s = String::from_utf8_lossy(b).into_owned();
            // The verbose protocol carries every scalar as text;
            // re-type numerals and booleans.
            if let Ok(i) = s.parse::<i64>() {
                Value::Int(i)
            } else if let Ok(f) = s.parse::<f64>() {
                Value::Float(f)
            } else if s == "true" || s == "false" {
                Value::Bool(s == "true")
            } else {
                Value::Str(s)
            }
        }
        R::SimpleString(s) => Value::Str(s.clone()),
        R::Array(items) => Value::List(items.iter().map(cell_value).collect()),
        other => Value::Str(format!("{other:?}")),
    }
}

/// A value as an inline Cypher literal.
fn cypher_literal(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        other => format!(
            "'{}'",
            other.to_string().replace('\\', "\\\\").replace('\'', "\\'")
        ),
    }
}

/// A verbose entity cell (`[["id", 0], ["labels", […]],
/// ["properties", [[k, v]…]]]`) as its parts.
struct EntityCell {
    id: i64,
    labels: Vec<String>,
    rel_type: Option<String>,
    props: Vec<(String, Value)>,
}

fn parse_entity(v: &R) -> Option<EntityCell> {
    let R::Array(pairs) = v else { return None };
    let mut out = EntityCell {
        id: -1,
        labels: Vec::new(),
        rel_type: None,
        props: Vec::new(),
    };
    for p in pairs {
        let R::Array(kv) = p else { continue };
        let key = kv.first().and_then(text)?;
        match (key.as_str(), kv.get(1)) {
            ("id", Some(R::Int(i))) => out.id = *i,
            ("labels", Some(R::Array(ls))) => {
                out.labels = ls.iter().filter_map(text).collect();
            }
            ("type", Some(t)) => out.rel_type = text(t),
            ("properties", Some(R::Array(ps))) => {
                for pp in ps {
                    if let R::Array(pkv) = pp
                        && let Some(k) = pkv.first().and_then(text)
                        && let Some(vv) = pkv.get(1)
                    {
                        out.props.push((k, cell_value(vv)));
                    }
                }
            }
            _ => {}
        }
    }
    (out.id >= 0).then_some(out)
}

enum Kind {
    Root,
    Label { index: usize },
    Entity {
        gid: i64,
        labels: Vec<String>,
        props: Vec<(String, Value)>,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A FalkorDB graph, exposed as an arbor.
pub struct FalkorAdapter {
    conn: RefCell<redis::Connection>,
    graph: String,
    key: Vec<String>,
    labels: Vec<String>,
    rel_types: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    by_gid: RefCell<HashMap<i64, NodeId>>,
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl FalkorAdapter {
    /// Connect to `falkor://…`; the label catalog doubles as the
    /// probe.
    pub fn connect(target: &str) -> Result<Self, FalkorError> {
        let (scheme, rest) = if let Some(r) = target.strip_prefix("falkor://") {
            ("redis", r)
        } else if let Some(r) = target.strip_prefix("falkors://") {
            ("rediss", r)
        } else {
            return Err(FalkorError::Target(target.to_string()));
        };
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };
        let (hostpart, graph) = rest
            .rsplit_once('/')
            .filter(|(_, g)| !g.is_empty())
            .ok_or_else(|| FalkorError::Target(target.to_string()))?;
        let key = query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("key=").map(str::to_string))
        });
        let client =
            redis::Client::open(format!("{scheme}://{hostpart}")).map_err(api)?;
        let conn = client.get_connection().map_err(api)?;
        let adapter = FalkorAdapter {
            conn: RefCell::new(conn),
            graph: graph.to_string(),
            key: key
                .map(|k| k.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            labels: Vec::new(),
            rel_types: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_gid: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        };
        let mut labels: Vec<String> = adapter
            .cypher("CALL db.labels()")?
            .iter()
            .filter_map(|row| row.first().and_then(text))
            .collect();
        labels.sort();
        let mut rel_types: Vec<String> = adapter
            .cypher("CALL db.relationshipTypes()")?
            .iter()
            .filter_map(|row| row.first().and_then(text))
            .collect();
        rel_types.sort();
        let mut adapter = adapter;
        for (i, l) in labels.iter().enumerate() {
            adapter.nodes.get_mut().push(Node {
                kind: Kind::Label { index: i },
                name: Some(l.clone()),
                parent: Some(NodeId(0)),
                children: RefCell::new(None),
            });
        }
        adapter.labels = labels;
        adapter.rel_types = rel_types;
        Ok(adapter)
    }

    /// A human-readable locator: `/Label/name`.
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

    /// One read-only GRAPH.QUERY; the middle element of the reply
    /// is the row set.
    fn cypher(&self, stmt: &str) -> Result<Vec<Vec<R>>, FalkorError> {
        let reply: R = redis::cmd("GRAPH.RO_QUERY")
            .arg(&self.graph)
            .arg(stmt)
            .query(&mut self.conn.borrow_mut())
            .or_else(|_| {
                // Older servers lack RO_QUERY; the statement is
                // still read-only Cypher.
                redis::cmd("GRAPH.QUERY")
                    .arg(&self.graph)
                    .arg(stmt)
                    .query(&mut self.conn.borrow_mut())
            })
            .map_err(api)?;
        let R::Array(parts) = reply else {
            return Ok(Vec::new());
        };
        // [header, rows, stats] — a reply with no rows is
        // [header, stats].
        let rows = match parts.get(1) {
            Some(R::Array(rows)) if parts.len() >= 3 => rows.clone(),
            _ => Vec::new(),
        };
        Ok(rows
            .into_iter()
            .filter_map(|r| match r {
                R::Array(cells) => Some(cells),
                _ => None,
            })
            .collect())
    }

    /// Intern one fetched entity cell.
    fn intern(&self, cell: &R) -> Option<NodeId> {
        let e = parse_entity(cell)?;
        if let Some(&id) = self.by_gid.borrow().get(&e.id) {
            return Some(id);
        }
        let name = self
            .key
            .iter()
            .find_map(|k| {
                e.props
                    .iter()
                    .find(|(p, _)| p == k)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_else(|| e.id.to_string());
        let parent = e
            .labels
            .first()
            .and_then(|l| self.labels.iter().position(|x| x == l))
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Entity {
                gid: e.id,
                labels: e.labels,
                props: e.props,
            },
            name: Some(name),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_gid.borrow_mut().insert(e.id, id);
        Some(id)
    }

    /// One node's relationships, outgoing or incoming.
    fn edges(&self, node: NodeId, incoming: bool) -> Vec<(String, NodeId)> {
        let gid = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { gid, .. } => *gid,
            _ => return Vec::new(),
        };
        let arrow = if incoming { "<-[r]-" } else { "-[r]->" };
        let stmt = format!(
            "MATCH (n){arrow}(m) WHERE id(n) = {gid} RETURN r, m ORDER BY type(r), id(m)"
        );
        let rows = self.cypher(&stmt).unwrap_or_default();
        rows.iter()
            .filter_map(|row| {
                let e = parse_entity(row.first()?)?;
                let label = e.rel_type?;
                let other = self.intern(row.get(1)?)?;
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                self.edge_props
                    .borrow_mut()
                    .entry((source, label.clone(), target))
                    .or_insert(e.props);
                Some((label, other))
            })
            .collect()
    }
}

impl AstAdapter for FalkorAdapter {
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
        let label = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => {
                return (1..=self.labels.len()).map(|i| NodeId(i as u64)).collect();
            }
            Kind::Entity { .. } => return Vec::new(),
            Kind::Label { index } => self.labels[*index].clone(),
        };
        let order = if self.key.is_empty() {
            "id(m)".to_string()
        } else {
            format!("m.{}", self.key.join(", m."))
        };
        let stmt = format!("MATCH (m:`{label}`) RETURN m ORDER BY {order}");
        let rows = self.cypher(&stmt).unwrap_or_default();
        let ids: Vec<NodeId> = rows
            .iter()
            .filter_map(|row| self.intern(row.first()?))
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
            Kind::Label { .. } => vec!["label".to_string()],
            Kind::Entity { labels, .. } => labels.clone(),
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
            (Kind::Root, "labels") => Some(Value::List(
                self.labels.iter().map(|l| Value::Str(l.clone())).collect(),
            )),
            (Kind::Root, "rel-types") => Some(Value::List(
                self.rel_types
                    .iter()
                    .map(|t| Value::Str(t.clone()))
                    .collect(),
            )),
            (Kind::Label { index }, "n-rows") => {
                let stmt = format!(
                    "MATCH (m:`{}`) RETURN count(m)",
                    self.labels[*index]
                );
                drop(nodes);
                let rows = self.cypher(&stmt).ok()?;
                rows.first()?.first().map(cell_value)
            }
            (Kind::Entity { gid, .. }, "id") => Some(Value::Int(*gid)),
            (Kind::Entity { labels, .. }, "labels") => Some(Value::List(
                labels.iter().map(|l| Value::Str(l.clone())).collect(),
            )),
            _ => None,
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, false)
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, true)
    }

    /// Hint-form resolution against the target label's `?key=`
    /// property (or the internal id without one).
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let label = hint?;
        let value = self.property(node, property)?;
        let cond = match self.key.first() {
            Some(k) => format!("m.{k} = {}", cypher_literal(&value)),
            None => format!("id(m) = {}", cypher_literal(&value)),
        };
        let stmt = format!("MATCH (m:`{label}`) WHERE {cond} RETURN m LIMIT 1");
        let rows = self.cypher(&stmt).ok()?;
        self.intern(rows.first()?.first()?)
    }

    /// `$-::prop` — a relationship's own property, from the cache
    /// the edge fetch filled; a cold read refetches.
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
    fn entity_cells_parse() {
        let cell = R::Array(vec![
            R::Array(vec![R::BulkString(b"id".to_vec()), R::Int(7)]),
            R::Array(vec![
                R::BulkString(b"labels".to_vec()),
                R::Array(vec![R::BulkString(b"Person".to_vec())]),
            ]),
            R::Array(vec![
                R::BulkString(b"properties".to_vec()),
                R::Array(vec![R::Array(vec![
                    R::BulkString(b"name".to_vec()),
                    R::BulkString(b"Ada".to_vec()),
                ])]),
            ]),
        ]);
        let e = parse_entity(&cell).unwrap();
        assert_eq!(e.id, 7);
        assert_eq!(e.labels, ["Person"]);
        assert_eq!(e.props[0].0, "name");
    }

    #[test]
    fn scalars_retype() {
        assert_eq!(cell_value(&R::BulkString(b"42".to_vec())), Value::Int(42));
        assert_eq!(
            cell_value(&R::BulkString(b"1.5".to_vec())),
            Value::Float(1.5)
        );
        assert_eq!(
            cell_value(&R::BulkString(b"Ada".to_vec())),
            Value::Str("Ada".to_string())
        );
    }

    #[test]
    fn target_forms() {
        assert!(FalkorAdapter::connect("redis://x").is_err());
    }
}
