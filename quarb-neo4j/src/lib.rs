//! Neo4j property-graph adapter for the Quarb query engine.
//!
//! A property graph is the arbor model with the tree backbone left
//! implicit: nodes carry labels and properties, relationships are
//! typed edges. The adapter supplies the backbone — the root holds
//! one child per **label** (like a relational adapter's tables), a
//! label holds the nodes carrying it — and maps **relationships to
//! labeled crosslinks**: `->REPORTS_TO` follows outgoing
//! relationships of that type, `<-REPORTS_TO` incoming, `->*` any.
//! Properties are `::prop` projections, relational-style.
//!
//! A **multi-label node appears under each of its labels** (Cypher
//! `MATCH (n:Actor)` semantics — filtering to one label would
//! silently miss nodes), interned once by `elementId`; its canonical
//! parent (and locator) uses the first label in storage order, and
//! `traits()` carries all labels, so `<Employee>` filters work from
//! any path. Relationship **properties** answer the `$-` edge
//! accessor (`->FRIEND[$-::since > 2016]`): edges stay edges — the
//! adapter serves `link_property` from a cache filled as edges are
//! fetched, falling back to a direct lookup. Parallel edges sharing
//! source, type, and target collapse to one (documented limit).
//!
//! Loading is catalog-eager, rows-lazy: labels and relationship
//! types come from `CALL db.labels()` / `db.relationshipTypes()` at
//! connect (which doubles as the connection probe); a label's node
//! set materializes on first touch; links and backlinks are one
//! Cypher round-trip per node, targets interned on sight. The
//! adapter only ever sends read statements; it never writes.
//!
//! **Transport and target**: the HTTP transaction endpoint
//! (`POST /db/DBNAME/tx/commit`), same on Neo4j 4.x and 5.x.
//! `neo4j://[USER:PASS@]HOST[:7474][/DBNAME][?key=PROP]` — the
//! database defaults to `neo4j`, the port to 7474 (the HTTP port,
//! not Bolt's 7687). A password may also come from
//! `QUARB_NEO4J_PASS` (history-safe); a server started with
//! `NEO4J_AUTH=none` needs no credentials. `?key=PROP` names the
//! property whose value names each node (`/Person/42` by internal
//! id without it, `/Person/ada` with `?key=name`).

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a graph.
#[derive(Debug, thiserror::Error)]
pub enum Neo4jError {
    #[error("neo4j: {0}")]
    Api(String),
    #[error("neo4j target: {0} (expected neo4j://[USER:PASS@]HOST[:7474][/DBNAME][?key=PROP])")]
    Target(String),
}

/// A parsed `neo4j://` target.
#[derive(Debug, PartialEq)]
struct Target {
    host: String,
    port: u16,
    database: String,
    user: Option<String>,
    pass: Option<String>,
    /// `?key=PROP[,PROP…]` — the properties that name a node, in
    /// fallback order (first present wins).
    key: Option<String>,
}

fn parse_target(target: &str) -> Result<Target, Neo4jError> {
    let bad = || Neo4jError::Target(target.to_string());
    let rest = target.strip_prefix("neo4j://").ok_or_else(bad)?;
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
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
            None => (Some(c.to_string()), None),
        },
        None => (None, None),
    };
    let (hostport, database) = match rest.split_once('/') {
        Some((hp, db)) if !db.is_empty() => (hp, db.to_string()),
        Some((hp, _)) => (hp, "neo4j".to_string()),
        None => (rest, "neo4j".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().map_err(|_| bad())?),
        None => (hostport.to_string(), 7474),
    };
    if host.is_empty() {
        return Err(bad());
    }
    let key = query.and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("key=").map(str::to_string))
    });
    let pass = pass.or_else(|| std::env::var("QUARB_NEO4J_PASS").ok().filter(|p| !p.is_empty()));
    Ok(Target {
        host,
        port,
        database,
        user,
        pass,
        key,
    })
}

/// Standard base64 (RFC 4648, with padding) for the Basic-auth
/// header — small enough to keep dependency-free.
fn base64(input: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, &x) in idx.iter().enumerate() {
            out.push(if i <= chunk.len() {
                ABC[x as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// The HTTP transaction endpoint client: one auto-commit Cypher
/// statement per call, rows back as JSON.
struct Client {
    /// `http://HOST:PORT/db/DBNAME/tx/commit`
    url: String,
    /// The `Basic …` header value, when credentials were given.
    auth: Option<String>,
}

impl Client {
    fn cypher(&self, stmt: &str, params: Json) -> Result<Vec<Vec<Json>>, Neo4jError> {
        let body = serde_json::json!({
            "statements": [{ "statement": stmt, "parameters": params }]
        });
        let mut req = ureq::post(&self.url);
        if let Some(a) = &self.auth {
            req = req.set("Authorization", a);
        }
        let resp = req
            .send_json(body)
            .map_err(|e| Neo4jError::Api(e.to_string()))?;
        let json: Json = resp
            .into_json()
            .map_err(|e| Neo4jError::Api(format!("decoding response: {e}")))?;
        if let Some(errors) = json.pointer("/errors").and_then(|v| v.as_array())
            && let Some(first) = errors.first()
        {
            let code = first.pointer("/code").and_then(|v| v.as_str()).unwrap_or("");
            let msg = first
                .pointer("/message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(Neo4jError::Api(format!("{code}: {msg}")));
        }
        Ok(json
            .pointer("/results/0/data")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.pointer("/row").and_then(|v| v.as_array()).cloned())
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// A label name in backticks, backticks doubled.
fn bt(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// A JSON result cell as a Quarb value. Maps (points, temporal
/// values) take the text posture: their JSON rendering as a string.
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

/// A Quarb value as a Cypher parameter, type-preserving (the inverse
/// of [`cell_value`]): an integer stays a JSON number, so a `?key=`
/// match against an integer-typed property compares like against
/// like. Temporal, durational, quantital, and record values fall back
/// to their text form.
fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(n) => Json::from(*n),
        Value::Float(f) => Json::from(*f),
        Value::Str(s) => Json::String(s.clone()),
        Value::List(items) => Json::Array(items.iter().map(value_to_json).collect()),
        _ => Json::String(v.to_string()),
    }
}

/// The short display form of an `elementId`: its last `:` segment
/// (`"4:abc…:42"` → `"42"`, the internal id).
fn short_id(eid: &str) -> String {
    eid.rsplit(':').next().unwrap_or(eid).to_string()
}

/// What a node is.
enum Kind {
    Root,
    /// A label, indexed into the catalog.
    Label { index: usize },
    /// A graph node: its `elementId`, labels in storage order, and
    /// decoded properties.
    Entity {
        eid: String,
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

/// A Neo4j database, exposed as an arbor.
pub struct Neo4jAdapter {
    client: Client,
    /// `?key=` — the node-naming properties, in fallback order
    /// (first present on a node wins; a label whose nodes carry
    /// none falls back to the internal id).
    key: Vec<String>,
    /// The label catalog, sorted; label i is node i + 1.
    labels: Vec<String>,
    rel_types: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    /// elementId → interned entity node.
    by_eid: RefCell<HashMap<String, NodeId>>,
    /// Relationship properties, cached as edges are fetched:
    /// (source, type, target) → [(name, value)]. Parallel edges
    /// collapse to the first fetched (documented limit).
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl Neo4jAdapter {
    /// Connect to `neo4j://[USER:PASS@]HOST[:7474][/DB][?key=PROP]`.
    pub fn connect(target: &str) -> Result<Self, Neo4jError> {
        let t = parse_target(target)?;
        let auth = t.user.as_ref().map(|u| {
            let creds = format!("{}:{}", u, t.pass.as_deref().unwrap_or(""));
            format!("Basic {}", base64(creds.as_bytes()))
        });
        let client = Client {
            url: format!("http://{}:{}/db/{}/tx/commit", t.host, t.port, t.database),
            auth,
        };
        // The catalog doubles as the connection probe: errors
        // surface here, not mid-query.
        let mut labels: Vec<String> = client
            .cypher("CALL db.labels()", serde_json::json!({}))?
            .into_iter()
            .filter_map(|row| row.first().and_then(|v| v.as_str().map(str::to_string)))
            .collect();
        labels.sort();
        let mut rel_types: Vec<String> = client
            .cypher("CALL db.relationshipTypes()", serde_json::json!({}))?
            .into_iter()
            .filter_map(|row| row.first().and_then(|v| v.as_str().map(str::to_string)))
            .collect();
        rel_types.sort();
        let mut nodes = vec![Node {
            kind: Kind::Root,
            name: None,
            parent: None,
            children: RefCell::new(None),
        }];
        for (i, label) in labels.iter().enumerate() {
            nodes.push(Node {
                kind: Kind::Label { index: i },
                name: Some(label.clone()),
                parent: Some(NodeId(0)),
                children: RefCell::new(None),
            });
        }
        Ok(Neo4jAdapter {
            client,
            key: t
                .key
                .map(|k| k.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            labels,
            rel_types,
            nodes: RefCell::new(nodes),
            by_eid: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        })
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

    /// The RETURN triple every node fetch shares.
    const NODE_ROW: &'static str = "elementId(m), labels(m), properties(m)";

    /// Intern one fetched node row (`elementId, labels, properties`),
    /// reusing the entity a previous fetch minted.
    fn intern(&self, row: &[Json]) -> Option<NodeId> {
        let eid = row.first()?.as_str()?.to_string();
        if let Some(&id) = self.by_eid.borrow().get(&eid) {
            return Some(id);
        }
        let labels: Vec<String> = row
            .get(1)?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let props: Vec<(String, Value)> = row
            .get(2)?
            .as_object()
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
            .unwrap_or_else(|| short_id(&eid));
        // Canonical parent: the first label in storage order (the
        // node is still listed under every label it carries).
        let parent = labels
            .first()
            .and_then(|l| self.labels.iter().position(|x| x == l))
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Entity { eid: eid.clone(), labels, props },
            name: Some(name),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_eid.borrow_mut().insert(eid, id);
        Some(id)
    }

    /// One node's relationships, outgoing or incoming, as
    /// `(type, interned target)` pairs.
    fn edges(&self, node: NodeId, incoming: bool) -> Vec<(String, NodeId)> {
        let eid = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { eid, .. } => eid.clone(),
            _ => return Vec::new(),
        };
        let arrow = if incoming { "<-[r]-" } else { "-[r]->" };
        let stmt = format!(
            "MATCH (n) WHERE elementId(n) = $eid MATCH (n){arrow}(m) \
             RETURN type(r), properties(r), {} ORDER BY type(r), id(m)",
            Self::NODE_ROW
        );
        let rows = self
            .client
            .cypher(&stmt, serde_json::json!({ "eid": eid }))
            .unwrap_or_default();
        rows.iter()
            .filter_map(|row| {
                let label = row.first()?.as_str()?.to_string();
                let other = self.intern(&row[2..])?;
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                let props: Vec<(String, Value)> = row
                    .get(1)
                    .and_then(|v| v.as_object())
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

impl AstAdapter for Neo4jAdapter {
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
            let props: Vec<String> = self.key.iter().map(|k| format!("m.{}", bt(k))).collect();
            format!("coalesce({}, toString(id(m)))", props.join(", "))
        };
        let stmt = format!(
            "MATCH (m:{}) RETURN {} ORDER BY {order}",
            bt(&label),
            Self::NODE_ROW
        );
        let rows = self
            .client
            .cypher(&stmt, serde_json::json!({}))
            .unwrap_or_default();
        let ids: Vec<NodeId> = rows.iter().filter_map(|row| self.intern(row)).collect();
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// An entity's traits are its labels (all of them, so
    /// `<Employee>` filters work from any path); a label node is a
    /// `<label>`.
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
                let stmt = format!("MATCH (m:{}) RETURN count(m)", bt(&self.labels[*index]));
                let rows = self.client.cypher(&stmt, serde_json::json!({})).ok()?;
                rows.first()?.first().map(cell_value)
            }
            (Kind::Label { .. }, "loaded") => Some(Value::Bool(
                nodes[node.0 as usize].children.borrow().is_some(),
            )),
            (Kind::Entity { eid, .. }, "element-id") => Some(Value::Str(eid.clone())),
            (Kind::Entity { labels, .. }, "labels") => Some(Value::List(
                labels.iter().map(|l| Value::Str(l.clone())).collect(),
            )),
            (Kind::Entity { labels, .. }, "label") => {
                labels.first().map(|l| Value::Str(l.clone()))
            }
            (Kind::Entity { eid, .. }, "out-degree" | "in-degree") => {
                let arrow = if key == "out-degree" { "-->" } else { "<--" };
                let stmt = format!(
                    "MATCH (n) WHERE elementId(n) = $eid RETURN COUNT {{ (n){arrow}() }}"
                );
                let eid = eid.clone();
                drop(nodes);
                let rows = self
                    .client
                    .cypher(&stmt, serde_json::json!({ "eid": eid }))
                    .ok()?;
                rows.first()?.first().map(cell_value)
            }
            _ => None,
        }
    }

    /// Outgoing relationships as `(type, target)` crosslinks.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, false)
    }

    /// Incoming relationships as `(type, source)` crosslinks.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, true)
    }

    /// Hint-form resolution: `::boss_id~>Person` matches the
    /// property's value against the target label's `?key=` property
    /// (or, without one, the internal id).
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let label = hint?;
        let value = self.property(node, property)?;
        let (cond, param) = match self.key.first() {
            Some(k) => (format!("m.{} = $v", bt(k)), value_to_json(&value)),
            None => (
                "id(m) = toInteger($v)".to_string(),
                Json::String(value.to_string()),
            ),
        };
        let stmt = format!(
            "MATCH (m:{}) WHERE {cond} RETURN {} LIMIT 1",
            bt(label),
            Self::NODE_ROW
        );
        let rows = self
            .client
            .cypher(&stmt, serde_json::json!({ "v": param }))
            .ok()?;
        self.intern(rows.first()?)
    }

    /// `$-::prop` — a relationship's own property. Served from the
    /// cache the edge fetch filled; a cold read (an edge never
    /// listed this session) asks the server directly.
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
        let (s_eid, t_eid) = {
            let nodes = self.nodes.borrow();
            let eid_of = |n: NodeId| match &nodes[n.0 as usize].kind {
                Kind::Entity { eid, .. } => Some(eid.clone()),
                _ => None,
            };
            (eid_of(source)?, eid_of(target)?)
        };
        let stmt = format!(
            "MATCH (a)-[r:{}]->(b) WHERE elementId(a) = $s AND elementId(b) = $t \
             RETURN properties(r) LIMIT 1",
            bt(label)
        );
        let rows = self
            .client
            .cypher(&stmt, serde_json::json!({ "s": s_eid, "t": t_eid }))
            .ok()?;
        let props: Vec<(String, Value)> = rows
            .first()?
            .first()?
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), cell_value(v)))
            .collect();
        let out = props.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        self.edge_props
            .borrow_mut()
            .insert((source, label.to_string(), target), props);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_forms() {
        let t = parse_target("neo4j://localhost").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.database.as_str()),
            ("localhost", 7474, "neo4j")
        );
        assert_eq!(t.user, None);
        assert_eq!(t.key, None);

        let t = parse_target("neo4j://ada:pw@db.example:7475/movies?key=id").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.database.as_str()),
            ("db.example", 7475, "movies")
        );
        assert_eq!(t.user.as_deref(), Some("ada"));
        assert_eq!(t.pass.as_deref(), Some("pw"));
        assert_eq!(t.key.as_deref(), Some("id"));

        // trailing slash → default database
        let t = parse_target("neo4j://localhost/").unwrap();
        assert_eq!(t.database, "neo4j");

        assert!(parse_target("bolt://localhost").is_err());
        assert!(parse_target("neo4j://").is_err());
        assert!(parse_target("neo4j://host:port").is_err());
    }

    #[test]
    fn cell_values() {
        assert_eq!(cell_value(&serde_json::json!(42)), Value::Int(42));
        assert_eq!(cell_value(&serde_json::json!(1.5)), Value::Float(1.5));
        assert_eq!(cell_value(&serde_json::json!(true)), Value::Bool(true));
        assert_eq!(
            cell_value(&serde_json::json!("x")),
            Value::Str("x".to_string())
        );
        assert_eq!(cell_value(&serde_json::json!(null)), Value::Null);
        assert_eq!(
            cell_value(&serde_json::json!([1, 2])),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        // maps take the text posture
        assert_eq!(
            cell_value(&serde_json::json!({"a": 1})),
            Value::Str("{\"a\":1}".to_string())
        );
    }

    #[test]
    fn value_params_preserve_type() {
        // A `?key=` reference must send its value under the source
        // type, not stringified: an integer boss_id has to reach the
        // server as JSON `42`, so `m.id = $v` matches an integer key.
        assert_eq!(value_to_json(&Value::Int(42)), serde_json::json!(42));
        assert!(value_to_json(&Value::Int(42)).is_number());
        assert_eq!(value_to_json(&Value::Float(1.5)), serde_json::json!(1.5));
        assert_eq!(value_to_json(&Value::Bool(true)), serde_json::json!(true));
        assert_eq!(
            value_to_json(&Value::Str("ada".to_string())),
            serde_json::json!("ada")
        );
        assert_eq!(value_to_json(&Value::Null), serde_json::json!(null));
    }

    #[test]
    fn helpers() {
        assert_eq!(short_id("4:abc-def:42"), "42");
        assert_eq!(short_id("plain"), "plain");
        assert_eq!(bt("Person"), "`Person`");
        assert_eq!(bt("od`d"), "`od``d`");
        assert_eq!(base64(b"neo4j:secret"), "bmVvNGo6c2VjcmV0");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }
}
