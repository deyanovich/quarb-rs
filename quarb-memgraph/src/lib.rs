//! Memgraph adapter for the Quarb query engine.
//!
//! Memgraph is Cypher-compatible but speaks **only Bolt** — no
//! HTTP transaction endpoint — so this adapter carries the Neo4j
//! mapping over a Bolt driver (neo4rs, pure Rust): the root
//! holds one child per **label**, a label holds its nodes, and
//! **relationship types become typed crosslinks** (`->KNOWS`,
//! `<-KNOWS`, `->*`) with relationship properties answering the
//! `$-` edge accessor. `?key=PROP` names nodes by a property;
//! without it the internal id names the node.
//!
//! The catalog comes from a `DISTINCT labels(n)` sweep (portable
//! across Memgraph versions); rows load lazily per label; only
//! read statements are ever sent.
//!
//! **Target**:
//! `memgraph://[USER:PASS@]HOST[:7687][?key=PROP&db=NAME]` —
//! the Bolt port; anonymous when Memgraph runs without auth;
//! the database defaults to `memgraph`.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading Memgraph.
#[derive(Debug, thiserror::Error)]
pub enum MemgraphError {
    #[error("memgraph: {0}")]
    Api(String),
    #[error("memgraph target: {0} (expected memgraph://[USER:PASS@]HOST[:7687][?key=PROP])")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> MemgraphError {
    MemgraphError::Api(e.to_string())
}

/// A JSON-decoded Bolt property as a Quarb value.
fn cell_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(items) => Value::List(items.iter().map(cell_value).collect()),
        serde_json::Value::Object(_) => Value::Str(v.to_string()),
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

/// All of a Bolt entity's properties as (name, value) pairs.
fn props_of_node(n: &neo4rs::Node) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = n
        .keys()
        .into_iter()
        .filter_map(|k| {
            n.get::<serde_json::Value>(k)
                .ok()
                .map(|v| (k.to_string(), cell_value(&v)))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn props_of_rel(r: &neo4rs::Relation) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = r
        .keys()
        .into_iter()
        .filter_map(|k| {
            r.get::<serde_json::Value>(k)
                .ok()
                .map(|v| (k.to_string(), cell_value(&v)))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
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

/// A Memgraph database, exposed as an arbor.
pub struct MemgraphAdapter {
    rt: tokio::runtime::Runtime,
    graph: neo4rs::Graph,
    key: Vec<String>,
    labels: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    by_gid: RefCell<HashMap<i64, NodeId>>,
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl MemgraphAdapter {
    /// Connect to `memgraph://…`; the label sweep doubles as the
    /// probe.
    pub fn connect(target: &str) -> Result<Self, MemgraphError> {
        let rest = target
            .strip_prefix("memgraph://")
            .ok_or_else(|| MemgraphError::Target(target.to_string()))?;
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };
        let (creds, hostport) = match rest.rsplit_once('@') {
            Some((c, r)) => (Some(c), r),
            None => (None, rest),
        };
        if hostport.is_empty() {
            return Err(MemgraphError::Target(target.to_string()));
        }
        let (user, pass) = match creds.map(|c| c.split_once(':')) {
            Some(Some((u, p))) => (u.to_string(), p.to_string()),
            Some(None) => (creds.unwrap_or_default().to_string(), String::new()),
            None => (String::new(), String::new()),
        };
        let uri = if hostport.contains(':') {
            hostport.to_string()
        } else {
            format!("{hostport}:7687")
        };
        let key = query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("key=").map(str::to_string))
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(api)?;
        // Memgraph's default (and usually only) database is
        // "memgraph"; the driver would otherwise ask for "neo4j".
        let db = query
            .and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("db=").map(str::to_string))
            })
            .unwrap_or_else(|| "memgraph".to_string());
        let config = neo4rs::ConfigBuilder::default()
            .uri(&uri)
            .user(&user)
            .password(&pass)
            .db(db.as_str())
            .build()
            .map_err(api)?;
        let graph = rt.block_on(neo4rs::Graph::connect(config)).map_err(api)?;
        let adapter = MemgraphAdapter {
            rt,
            graph,
            key: key
                .map(|k| k.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            labels: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_gid: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        };
        let mut labels: Vec<String> = Vec::new();
        {
            let rows = adapter
                .rows("MATCH (n) UNWIND labels(n) AS l RETURN DISTINCT l ORDER BY l")?;
            for row in rows {
                if let Ok(l) = row.get::<String>("l") {
                    labels.push(l);
                }
            }
        }
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

    /// Run one read statement, all rows collected.
    fn rows(&self, stmt: &str) -> Result<Vec<neo4rs::Row>, MemgraphError> {
        self.rt
            .block_on(async {
                let mut stream = self.graph.execute(neo4rs::query(stmt)).await?;
                let mut out = Vec::new();
                while let Some(row) = stream.next().await? {
                    out.push(row);
                }
                Ok::<_, neo4rs::Error>(out)
            })
            .map_err(api)
    }

    /// Intern one fetched Bolt node.
    fn intern(&self, n: &neo4rs::Node) -> Option<NodeId> {
        let gid = n.id();
        if let Some(&id) = self.by_gid.borrow().get(&gid) {
            return Some(id);
        }
        let labels: Vec<String> = n.labels().into_iter().map(str::to_string).collect();
        let props = props_of_node(n);
        let name = self
            .key
            .iter()
            .find_map(|k| {
                props
                    .iter()
                    .find(|(p, _)| p == k)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_else(|| gid.to_string());
        let parent = labels
            .first()
            .and_then(|l| self.labels.iter().position(|x| x == l))
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Entity { gid, labels, props },
            name: Some(name),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_gid.borrow_mut().insert(gid, id);
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
        let rows = self.rows(&stmt).unwrap_or_default();
        rows.iter()
            .filter_map(|row| {
                let r: neo4rs::Relation = row.get("r").ok()?;
                let m: neo4rs::Node = row.get("m").ok()?;
                let label = r.typ().to_string();
                let other = self.intern(&m)?;
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                self.edge_props
                    .borrow_mut()
                    .entry((source, label.clone(), target))
                    .or_insert_with(|| props_of_rel(&r));
                Some((label, other))
            })
            .collect()
    }
}

impl AstAdapter for MemgraphAdapter {
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
            format!("m.`{}`", self.key.join("`, m.`"))
        };
        let stmt = format!("MATCH (m:`{label}`) RETURN m ORDER BY {order}");
        let rows = self.rows(&stmt).unwrap_or_default();
        let ids: Vec<NodeId> = rows
            .iter()
            .filter_map(|row| self.intern(&row.get::<neo4rs::Node>("m").ok()?))
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
            (Kind::Label { index }, "n-rows") => {
                let stmt = format!(
                    "MATCH (m:`{}`) RETURN count(m) AS c",
                    self.labels[*index]
                );
                drop(nodes);
                let rows = self.rows(&stmt).ok()?;
                rows.first()?.get::<i64>("c").ok().map(Value::Int)
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
            Some(k) => format!("m.`{k}` = {}", cypher_literal(&value)),
            None => format!("id(m) = {}", cypher_literal(&value)),
        };
        let stmt = format!("MATCH (m:`{label}`) WHERE {cond} RETURN m LIMIT 1");
        let rows = self.rows(&stmt).ok()?;
        self.intern(&rows.first()?.get::<neo4rs::Node>("m").ok()?)
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
    fn literals() {
        assert_eq!(cypher_literal(&Value::Int(7)), "7");
        assert_eq!(cypher_literal(&Value::Str("O'Slo".into())), r"'O\'Slo'");
    }

    #[test]
    fn target_scheme() {
        assert!(MemgraphAdapter::connect("bolt://x").is_err());
    }
}
