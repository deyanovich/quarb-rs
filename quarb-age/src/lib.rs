//! Apache AGE adapter for the Quarb query engine.
//!
//! AGE puts an openCypher property graph *inside PostgreSQL* —
//! the graph lives in your relational database, one extension
//! away. The adapter maps it exactly as the Neo4j adapter maps
//! its graph: the root holds one child per **vertex label** (the
//! tables), a label holds its vertices, and **edge labels become
//! typed crosslinks** — `->LIVES_IN` outgoing, `<-LIVES_IN`
//! incoming, `->*` any — with edge properties answering the `$-`
//! accessor.
//!
//! `?key=PROP[,PROP…]` names vertices by a property (first
//! present wins); without it a vertex is named by its graph id.
//! Loading is catalog-eager, rows-lazy; only read statements are
//! ever sent.
//!
//! **Transport**: the ordinary PostgreSQL wire (tokio-postgres,
//! text protocol — agtype values arrive in their text form and
//! are parsed here). Each connection runs `LOAD 'age'` and sets
//! the `ag_catalog` search path.
//!
//! **Target**:
//! `age://[USER[:PASS]@]HOST[:PORT]/DB/GRAPH[?key=PROP]` — the
//! user defaults to `postgres`, the port to 5432; a password may
//! also come from `QUARB_AGE_PASS` or `PGPASSWORD`.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading an AGE graph.
#[derive(Debug, thiserror::Error)]
pub enum AgeError {
    #[error("age: {0}")]
    Api(String),
    #[error("age target: {0} (expected age://[USER[:PASS]@]HOST[:PORT]/DB/GRAPH[?key=PROP])")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> AgeError {
    AgeError::Api(e.to_string())
}

/// A parsed `age://` target.
#[derive(Debug, PartialEq)]
struct Target {
    host: String,
    port: u16,
    database: String,
    graph: String,
    user: String,
    pass: Option<String>,
    key: Option<String>,
}

fn parse_target(target: &str) -> Result<Target, AgeError> {
    let bad = || AgeError::Target(target.to_string());
    let rest = target.strip_prefix("age://").ok_or_else(bad)?;
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let (creds, rest) = match rest.rsplit_once('@') {
        Some((c, r)) => (Some(c), r),
        None => (None, rest),
    };
    let (user, pass) = match creds {
        Some(c) => match c.split_once(':') {
            Some((u, p)) => (u.to_string(), Some(p.to_string())),
            None => (c.to_string(), None),
        },
        None => ("postgres".to_string(), None),
    };
    let mut parts = rest.splitn(3, '/');
    let hostport = parts.next().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let database = parts.next().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let graph = parts.next().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().map_err(|_| bad())?),
        None => (hostport.to_string(), 5432),
    };
    let key = query.and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("key=").map(str::to_string))
    });
    let pass = pass
        .or_else(|| std::env::var("QUARB_AGE_PASS").ok().filter(|p| !p.is_empty()))
        .or_else(|| std::env::var("PGPASSWORD").ok().filter(|p| !p.is_empty()));
    Ok(Target {
        host,
        port,
        database: database.to_string(),
        graph: graph.to_string(),
        user,
        pass,
        key,
    })
}

/// Strip an agtype annotation suffix (`::vertex`, `::edge`,
/// `::numeric`, …) and parse the remaining JSON.
fn agtype_json(text: &str) -> Option<Json> {
    let t = text.trim();
    let t = match t.rfind("::") {
        Some(i) if t[i + 2..].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => &t[..i],
        _ => t,
    };
    serde_json::from_str(t).ok()
}

/// A JSON property value as a Quarb value.
fn cell_value(v: &Json) -> Value {
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(cell_value).collect()),
        Json::Object(_) => Value::Str(v.to_string()),
    }
}

/// A value as an inline Cypher literal (AGE's cypher() takes the
/// statement as a dollar-quoted string, so values are inlined with
/// string escaping).
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

enum Kind {
    Root,
    Label { index: usize },
    /// A vertex: its graph id, label, and decoded properties.
    Entity {
        gid: i64,
        label: String,
        props: Vec<(String, Value)>,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// An AGE graph, exposed as an arbor.
pub struct AgeAdapter {
    rt: tokio::runtime::Runtime,
    client: tokio_postgres::Client,
    graph: String,
    key: Vec<String>,
    labels: Vec<String>,
    edge_labels: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    by_gid: RefCell<HashMap<i64, NodeId>>,
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl AgeAdapter {
    /// Connect to `age://…`; the label catalog doubles as the
    /// probe.
    pub fn connect(target: &str) -> Result<Self, AgeError> {
        let t = parse_target(target)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(api)?;
        let mut config = tokio_postgres::Config::new();
        config
            .host(&t.host)
            .port(t.port)
            .dbname(&t.database)
            .user(&t.user);
        if let Some(p) = &t.pass {
            config.password(p);
        }
        let client = rt
            .block_on(async {
                let (client, connection) = config.connect(tokio_postgres::NoTls).await?;
                tokio::spawn(connection);
                client
                    .batch_execute(
                        "LOAD 'age'; SET search_path = ag_catalog, \"$user\", public;",
                    )
                    .await?;
                Ok::<_, tokio_postgres::Error>(client)
            })
            .map_err(api)?;
        let adapter = AgeAdapter {
            rt,
            client,
            graph: t.graph.clone(),
            key: t
                .key
                .map(|k| k.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            labels: Vec::new(),
            edge_labels: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_gid: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        };
        // The label catalog (internal `_…` labels hidden).
        let sql = format!(
            "SELECT name, kind FROM ag_catalog.ag_label WHERE graph = \
             (SELECT graphid FROM ag_catalog.ag_graph WHERE name = '{}') \
             AND name NOT LIKE '\\_%' ORDER BY name",
            t.graph.replace('\'', "''")
        );
        let rows = adapter.simple(&sql)?;
        let mut labels = Vec::new();
        let mut edge_labels = Vec::new();
        for r in &rows {
            match (r.first().map(String::as_str), r.get(1).map(String::as_str)) {
                (Some(n), Some("v")) => labels.push(n.to_string()),
                (Some(n), Some("e")) => edge_labels.push(n.to_string()),
                _ => {}
            }
        }
        if labels.is_empty() && edge_labels.is_empty() && rows.is_empty() {
            // Distinguish "empty graph" from "no such graph".
            let probe = adapter.simple(&format!(
                "SELECT 1 FROM ag_catalog.ag_graph WHERE name = '{}'",
                t.graph.replace('\'', "''")
            ))?;
            if probe.is_empty() {
                return Err(AgeError::Api(format!("no graph named '{}'", t.graph)));
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
        adapter.edge_labels = edge_labels;
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

    /// Run SQL over the text protocol; rows come back as strings.
    fn simple(&self, sql: &str) -> Result<Vec<Vec<String>>, AgeError> {
        let messages = self
            .rt
            .block_on(self.client.simple_query(sql))
            .map_err(api)?;
        Ok(messages
            .into_iter()
            .filter_map(|m| match m {
                tokio_postgres::SimpleQueryMessage::Row(r) => Some(
                    (0..r.len())
                        .map(|i| r.get(i).unwrap_or_default().to_string())
                        .collect(),
                ),
                _ => None,
            })
            .collect())
    }

    /// Run a Cypher statement through `cypher()`, `cols` agtype
    /// columns per row.
    fn cypher(&self, stmt: &str, cols: usize) -> Result<Vec<Vec<String>>, AgeError> {
        let as_list: Vec<String> = (0..cols).map(|i| format!("c{i} agtype")).collect();
        let sql = format!(
            "SELECT * FROM ag_catalog.cypher('{}', $quarb${}$quarb$) AS ({})",
            self.graph.replace('\'', "''"),
            stmt,
            as_list.join(", ")
        );
        self.simple(&sql)
    }

    /// Intern one vertex from its agtype JSON.
    fn intern(&self, j: &Json) -> Option<NodeId> {
        let gid = j.pointer("/id")?.as_i64()?;
        if let Some(&id) = self.by_gid.borrow().get(&gid) {
            return Some(id);
        }
        let label = j.pointer("/label")?.as_str()?.to_string();
        let props: Vec<(String, Value)> = j
            .pointer("/properties")
            .and_then(|p| p.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), cell_value(v))).collect())
            .unwrap_or_default();
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
        let parent = self
            .labels
            .iter()
            .position(|l| l == &label)
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Entity { gid, label, props },
            name: Some(name),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_gid.borrow_mut().insert(gid, id);
        Some(id)
    }

    /// One vertex's edges, outgoing or incoming.
    fn edges(&self, node: NodeId, incoming: bool) -> Vec<(String, NodeId)> {
        let gid = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { gid, .. } => *gid,
            _ => return Vec::new(),
        };
        let arrow = if incoming { "<-[r]-" } else { "-[r]->" };
        let stmt =
            format!("MATCH (n){arrow}(m) WHERE id(n) = {gid} RETURN r, m ORDER BY id(r)");
        let rows = self.cypher(&stmt, 2).unwrap_or_default();
        rows.iter()
            .filter_map(|row| {
                let edge = agtype_json(row.first()?)?;
                let vert = agtype_json(row.get(1)?)?;
                let label = edge.pointer("/label")?.as_str()?.to_string();
                let other = self.intern(&vert)?;
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                let props: Vec<(String, Value)> = edge
                    .pointer("/properties")
                    .and_then(|p| p.as_object())
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), cell_value(v))).collect())
                    .unwrap_or_default();
                self.edge_props
                    .borrow_mut()
                    .entry((source, label.clone(), target))
                    .or_insert(props);
                Some((label, other))
            })
            .collect()
    }
}

impl AstAdapter for AgeAdapter {
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
            let props: Vec<String> = self.key.iter().map(|k| format!("m.{k}")).collect();
            format!("coalesce({}, id(m))", props.join(", "))
        };
        let stmt = format!("MATCH (m:{label}) RETURN m ORDER BY {order}");
        let rows = self.cypher(&stmt, 1).unwrap_or_default();
        let ids: Vec<NodeId> = rows
            .iter()
            .filter_map(|row| self.intern(&agtype_json(row.first()?)?))
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
            Kind::Entity { label, .. } => vec![label.clone()],
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
                self.edge_labels
                    .iter()
                    .map(|l| Value::Str(l.clone()))
                    .collect(),
            )),
            (Kind::Label { index }, "n-rows") => {
                let stmt = format!("MATCH (m:{}) RETURN count(m)", self.labels[*index]);
                drop(nodes);
                let rows = self.cypher(&stmt, 1).ok()?;
                agtype_json(rows.first()?.first()?).map(|j| cell_value(&j))
            }
            (Kind::Entity { gid, .. }, "id") => Some(Value::Int(*gid)),
            (Kind::Entity { label, .. }, "label") => Some(Value::Str(label.clone())),
            _ => None,
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, false)
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, true)
    }

    /// Hint-form resolution: `::city~>City` matches the property's
    /// value against the target label's `?key=` property (or the
    /// graph id without one).
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let label = hint?;
        let value = self.property(node, property)?;
        let cond = match self.key.first() {
            Some(k) => format!("m.{k} = {}", cypher_literal(&value)),
            None => format!("id(m) = {}", cypher_literal(&value)),
        };
        let stmt = format!("MATCH (m:{label}) WHERE {cond} RETURN m LIMIT 1");
        let rows = self.cypher(&stmt, 1).ok()?;
        self.intern(&agtype_json(rows.first()?.first()?)?)
    }

    /// `$-::prop` — an edge's own property, from the cache the edge
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
    fn target_forms() {
        let t = parse_target("age://localhost/postgres/office").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.database.as_str(), t.graph.as_str()),
            ("localhost", 5432, "postgres", "office")
        );
        assert_eq!(t.user, "postgres");

        let t = parse_target("age://ada:pw@db:15432/app/social?key=name").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.user.as_str(), t.pass.as_deref()),
            ("db", 15432, "ada", Some("pw"))
        );
        assert_eq!(t.key.as_deref(), Some("name"));

        assert!(parse_target("age://localhost/onlydb").is_err());
        assert!(parse_target("postgres://x/y/z").is_err());
    }

    #[test]
    fn agtype_parsing() {
        let v = agtype_json(
            r#"{"id": 42, "label": "City", "properties": {"name": "Oslo"}}::vertex"#,
        )
        .unwrap();
        assert_eq!(v.pointer("/properties/name").unwrap(), "Oslo");
        assert_eq!(agtype_json("3").unwrap(), serde_json::json!(3));
        assert_eq!(agtype_json("1.5::numeric").unwrap(), serde_json::json!(1.5));
        // a `::` inside a string is not an annotation
        assert_eq!(
            agtype_json(r#""a::b""#).unwrap(),
            serde_json::json!("a::b")
        );
    }

    #[test]
    fn literals_escape() {
        assert_eq!(cypher_literal(&Value::Int(7)), "7");
        assert_eq!(
            cypher_literal(&Value::Str("O'Slo".into())),
            r"'O\'Slo'"
        );
    }
}
