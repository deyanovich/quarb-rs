//! ArangoDB adapter for the Quarb query engine.
//!
//! ArangoDB's multi-model is almost embarrassingly arboreal:
//! **document collections** are tables of `_key`-named JSON
//! documents, and **edge collections** — documents with `_from`
//! and `_to` — are a typed crosslink fabric. The adapter maps
//! exactly that: the root holds one child per document
//! collection, documents descend as JSON subtrees (the
//! dual-exposure doctrine applies), and each edge collection's
//! name becomes a link label: `->works_in` follows its edges
//! out, `<-works_in` back, with edge-document attributes
//! answering the `$-` accessor.
//!
//! System fields stay out of the tree and surface as metadata
//! (`;;;id`, `;;;rev`; the `_key` is the node's name). Resolve
//! follows the `_id` convention: a value like `cities/oslo`
//! resolves hint-less; a bare key resolves with the collection
//! hint (`::city~>cities`).
//!
//! Listings are `_key`-sorted (deterministic); documents load
//! lazily per collection; only read AQL is ever sent.
//!
//! **Target**: `arango://[USER:PASS@]HOST[:8529]/DB` — basic
//! auth; a password may also come from `QUARB_ARANGO_PASS`.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading ArangoDB.
#[derive(Debug, thiserror::Error)]
pub enum ArangoError {
    #[error("arangodb: {0}")]
    Api(String),
    #[error("arangodb target: {0} (expected arango://[USER:PASS@]HOST[:8529]/DB)")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> ArangoError {
    ArangoError::Api(e.to_string())
}

/// Standard base64 for the Basic-auth header.
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

/// A JSON scalar as a Quarb value.
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

fn is_system(key: &str) -> bool {
    key.starts_with('_')
}

enum Kind {
    Root,
    Collection { index: usize },
    /// A document: its `_id`, collection, and body.
    Doc {
        id: String,
        rev: String,
        collection: String,
        body: Json,
    },
    /// A node inside a document's JSON subtree.
    Field { value: Json },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// An ArangoDB database, exposed as an arbor.
pub struct ArangoAdapter {
    base: String,
    auth: Option<String>,
    collections: Vec<String>,
    edge_collections: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    by_id: RefCell<HashMap<String, NodeId>>,
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl ArangoAdapter {
    /// Connect to `arango://…`; the collection catalog doubles as
    /// the probe.
    pub fn connect(target: &str) -> Result<Self, ArangoError> {
        let rest = target
            .strip_prefix("arango://")
            .ok_or_else(|| ArangoError::Target(target.to_string()))?;
        let (creds, rest) = match rest.rsplit_once('@') {
            Some((c, r)) => (Some(c), r),
            None => (None, rest),
        };
        let (hostport, db) = rest
            .split_once('/')
            .filter(|(h, d)| !h.is_empty() && !d.is_empty())
            .ok_or_else(|| ArangoError::Target(target.to_string()))?;
        let hostport = if hostport.contains(':') {
            hostport.to_string()
        } else {
            format!("{hostport}:8529")
        };
        let auth = creds.map(|c| {
            let full = match c.split_once(':') {
                Some(_) => c.to_string(),
                None => format!(
                    "{c}:{}",
                    std::env::var("QUARB_ARANGO_PASS").unwrap_or_default()
                ),
            };
            format!("Basic {}", base64(full.as_bytes()))
        });
        let adapter = ArangoAdapter {
            base: format!("http://{hostport}/_db/{db}"),
            auth,
            collections: Vec::new(),
            edge_collections: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_id: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        };
        let json = adapter.get("/_api/collection?excludeSystem=true")?;
        let mut collections = Vec::new();
        let mut edge_collections = Vec::new();
        if let Some(cs) = json.pointer("/result").and_then(|v| v.as_array()) {
            for c in cs {
                let name = c
                    .pointer("/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                match c.pointer("/type").and_then(|v| v.as_i64()) {
                    Some(2) => collections.push(name),
                    Some(3) => edge_collections.push(name),
                    _ => {}
                }
            }
        }
        collections.sort();
        edge_collections.sort();
        let mut adapter = adapter;
        for (i, c) in collections.iter().enumerate() {
            adapter.nodes.get_mut().push(Node {
                kind: Kind::Collection { index: i },
                name: Some(c.clone()),
                parent: Some(NodeId(0)),
                children: RefCell::new(None),
            });
        }
        adapter.collections = collections;
        adapter.edge_collections = edge_collections;
        Ok(adapter)
    }

    /// A human-readable locator: `/collection/_key…`.
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

    fn get(&self, path: &str) -> Result<Json, ArangoError> {
        let mut req = ureq::get(&format!("{}{path}", self.base));
        if let Some(a) = &self.auth {
            req = req.set("Authorization", a);
        }
        req.call()
            .map_err(|e| api(e))?
            .into_json()
            .map_err(|e| api(e))
    }

    /// Run an AQL query (with bind vars), draining the cursor.
    fn aql(&self, query: &str, binds: Json) -> Result<Vec<Json>, ArangoError> {
        let mut req = ureq::post(&format!("{}/_api/cursor", self.base));
        if let Some(a) = &self.auth {
            req = req.set("Authorization", a);
        }
        let mut json: Json = req
            .send_json(serde_json::json!({
                "query": query,
                "bindVars": binds,
                "batchSize": 1000,
            }))
            .map_err(|e| match e {
                ureq::Error::Status(code, r) => {
                    let text = r.into_string().unwrap_or_default();
                    let msg = serde_json::from_str::<Json>(&text)
                        .ok()
                        .and_then(|v| {
                            v.pointer("/errorMessage")
                                .and_then(|m| m.as_str())
                                .map(str::to_string)
                        })
                        .unwrap_or(text);
                    ArangoError::Api(format!("{code}: {msg}"))
                }
                other => api(other),
            })?
            .into_json()
            .map_err(|e| api(e))?;
        let mut out: Vec<Json> = json
            .pointer("/result")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        while json.pointer("/hasMore").and_then(|v| v.as_bool()) == Some(true) {
            let id = json
                .pointer("/id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let mut req = ureq::put(&format!("{}/_api/cursor/{id}", self.base));
            if let Some(a) = &self.auth {
                req = req.set("Authorization", a);
            }
            json = req
                .call()
                .map_err(|e| api(e))?
                .into_json()
                .map_err(|e| api(e))?;
            out.extend(
                json.pointer("/result")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        Ok(out)
    }

    /// Intern one fetched document.
    fn intern(&self, doc: &Json) -> Option<NodeId> {
        let id = doc.pointer("/_id")?.as_str()?.to_string();
        if let Some(&n) = self.by_id.borrow().get(&id) {
            return Some(n);
        }
        let key = doc.pointer("/_key")?.as_str()?.to_string();
        let rev = doc
            .pointer("/_rev")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let collection = id.split_once('/').map(|(c, _)| c.to_string())?;
        let parent = self
            .collections
            .iter()
            .position(|c| c == &collection)
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let nid = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Doc {
                id: id.clone(),
                rev,
                collection,
                body: doc.clone(),
            },
            name: Some(key),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_id.borrow_mut().insert(id, nid);
        Some(nid)
    }

    /// The JSON children of a value (system fields hidden at the
    /// document's top level).
    fn json_children(&self, v: &Json, parent: NodeId, top: bool) -> Vec<NodeId> {
        match v {
            Json::Object(o) => o
                .iter()
                .filter(|(k, _)| !(top && is_system(k)))
                .map(|(k, vv)| {
                    self.push(Node {
                        kind: Kind::Field { value: vv.clone() },
                        name: Some(k.clone()),
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Json::Array(items) => items
                .iter()
                .map(|vv| {
                    self.push(Node {
                        kind: Kind::Field { value: vv.clone() },
                        name: None,
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// One document's edges, outgoing or incoming, across every
    /// edge collection.
    fn edges(&self, node: NodeId, incoming: bool) -> Vec<(String, NodeId)> {
        let id = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { id, .. } => id.clone(),
            _ => return Vec::new(),
        };
        let (this_end, other_end) = if incoming {
            ("_to", "_from")
        } else {
            ("_from", "_to")
        };
        let mut out = Vec::new();
        for ec in &self.edge_collections {
            let query = format!(
                "FOR e IN @@ec FILTER e.{this_end} == @id SORT e._key \
                 RETURN {{ e: e, t: DOCUMENT(e.{other_end}) }}"
            );
            let rows = self
                .aql(&query, serde_json::json!({ "@ec": ec, "id": id }))
                .unwrap_or_default();
            for row in rows {
                let Some(t) = row.pointer("/t").filter(|t| !t.is_null()) else {
                    continue;
                };
                let Some(other) = self.intern(t) else { continue };
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                let props: Vec<(String, Value)> = row
                    .pointer("/e")
                    .and_then(|e| e.as_object())
                    .map(|m| {
                        m.iter()
                            .filter(|(k, _)| !is_system(k))
                            .map(|(k, v)| (k.clone(), cell_value(v)))
                            .collect()
                    })
                    .unwrap_or_default();
                self.edge_props
                    .borrow_mut()
                    .entry((source, ec.clone(), target))
                    .or_insert(props);
                out.push((ec.clone(), other));
            }
        }
        out
    }
}

impl AstAdapter for ArangoAdapter {
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
        enum Plan {
            Tables,
            Coll(String),
            Doc(Json),
            Field(Json),
            Leaf,
        }
        let plan = {
            match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Root => Plan::Tables,
                Kind::Collection { index } => Plan::Coll(self.collections[*index].clone()),
                Kind::Doc { body, .. } => Plan::Doc(body.clone()),
                Kind::Field { value } => match value {
                    Json::Object(_) | Json::Array(_) => Plan::Field(value.clone()),
                    _ => Plan::Leaf,
                },
            }
        };
        let made = match plan {
            Plan::Tables => {
                return (1..=self.collections.len())
                    .map(|i| NodeId(i as u64))
                    .collect();
            }
            Plan::Leaf => Vec::new(),
            Plan::Coll(name) => {
                let rows = self
                    .aql(
                        "FOR d IN @@c SORT d._key RETURN d",
                        serde_json::json!({ "@c": name }),
                    )
                    .unwrap_or_default();
                rows.iter().filter_map(|d| self.intern(d)).collect()
            }
            Plan::Doc(body) => self.json_children(&body, node, true),
            Plan::Field(value) => self.json_children(&value, node, false),
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(made.clone());
        made
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
            Kind::Collection { .. } => vec!["collection".to_string()],
            Kind::Doc { collection, .. } => vec![collection.clone()],
            Kind::Field { .. } => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { body, .. } => body
                .pointer(&format!("/{name}"))
                .filter(|v| !v.is_null() && !is_system(name))
                .map(cell_value),
            Kind::Field { value: Json::Object(o) } => {
                o.get(name).filter(|v| !v.is_null()).map(cell_value)
            }
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field { value } => match value {
                Json::Object(_) | Json::Array(_) => None,
                v => Some(cell_value(v)),
            },
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let nodes = self.nodes.borrow();
        match (&nodes[node.0 as usize].kind, key) {
            (Kind::Root, "collections") => Some(Value::List(
                self.collections
                    .iter()
                    .map(|c| Value::Str(c.clone()))
                    .collect(),
            )),
            (Kind::Root, "edge-collections") => Some(Value::List(
                self.edge_collections
                    .iter()
                    .map(|c| Value::Str(c.clone()))
                    .collect(),
            )),
            (Kind::Collection { index }, "n-rows") => {
                let name = self.collections[*index].clone();
                drop(nodes);
                let rows = self
                    .aql(
                        "RETURN LENGTH(@@c)",
                        serde_json::json!({ "@c": name }),
                    )
                    .ok()?;
                rows.first().map(cell_value)
            }
            (Kind::Doc { id, .. }, "id") => Some(Value::Str(id.clone())),
            (Kind::Doc { rev, .. }, "rev") => Some(Value::Str(rev.clone())),
            _ => None,
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, false)
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, true)
    }

    /// `_id`-convention resolution: hint-less when the value is a
    /// full `collection/key`, hinted (`~>cities`) for a bare key.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let value = self.property(node, property)?.to_string();
        let id = match hint {
            Some(h) => format!("{h}/{value}"),
            None => value,
        };
        if !id.contains('/') {
            return None;
        }
        if let Some(&n) = self.by_id.borrow().get(&id) {
            return Some(n);
        }
        let rows = self
            .aql("RETURN DOCUMENT(@id)", serde_json::json!({ "id": id }))
            .ok()?;
        let doc = rows.first().filter(|d| !d.is_null())?;
        self.intern(doc)
    }

    /// `$-::prop` — an edge document's own attribute.
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
    fn helpers() {
        assert_eq!(base64(b"root:pw"), "cm9vdDpwdw==");
        assert!(is_system("_key"));
        assert!(!is_system("name"));
        assert_eq!(cell_value(&serde_json::json!(42)), Value::Int(42));
    }

    #[test]
    fn target_scheme() {
        assert!(ArangoAdapter::connect("http://x/db").is_err());
        assert!(ArangoAdapter::connect("arango://hostonly").is_err());
    }
}
