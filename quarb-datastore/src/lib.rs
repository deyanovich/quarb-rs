//! Google Cloud Datastore (Firestore in Datastore mode) adapter
//! for the Quarb query engine.
//!
//! Datastore is kinds and entities — the closest of the document
//! stores to a relational shape: `/tracks/mars::title`, kinds as
//! the root's children, entities named by their key (name or
//! numeric id). Entity properties are typed values; `keyValue`
//! properties are *native references*, so `~>` needs neither a
//! schema nor a refs file: `::album~>::title` follows the key,
//! and `->` enumerates every key property as a labeled edge.
//!
//! Loading is lazy: the kind list comes from a `__kind__` query,
//! a kind's entities from a `runQuery` (cursor-paginated), one
//! kind on first touch, cached. Reads bill per entity — the lazy
//! model is the cost model. Read-only.
//!
//! **Auth and target**: `datastore://PROJECT[/DATABASE][?account=EMAIL]`
//! (omit the database for `(default)`). Token from
//! `QUARB_GCP_TOKEN`, else `gcloud auth print-access-token
//! [account]`. Named databases route via `x-goog-request-params`;
//! the adapter sends it always.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a database.
#[derive(Debug, thiserror::Error)]
pub enum DatastoreError {
    #[error("datastore: {0}")]
    Api(String),
    #[error("datastore target: {0} (expected datastore://PROJECT[/DATABASE][?account=EMAIL])")]
    Target(String),
}

fn gcp_token(account: Option<&str>) -> Result<String, String> {
    if let Ok(t) = std::env::var("QUARB_GCP_TOKEN")
        && !t.trim().is_empty()
    {
        return Ok(t.trim().to_string());
    }
    let mut cmd = std::process::Command::new("gcloud");
    cmd.args(["auth", "print-access-token"]);
    if let Some(a) = account {
        cmd.arg(a);
    }
    let out = cmd.output().map_err(|e| format!("running gcloud: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gcloud auth print-access-token failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A decoded property value.
#[derive(Clone)]
enum Prop {
    Scalar(Value),
    /// A key reference: `(kind, name-or-id)`.
    Key(String, String),
    Array(Vec<Prop>),
    /// A nested entity's properties.
    Entity(Vec<(String, Prop)>),
}

fn decode_key(k: &Json) -> Option<(String, String)> {
    let path = k.pointer("/path")?.as_array()?;
    let last = path.last()?;
    let kind = last.pointer("/kind")?.as_str()?.to_string();
    let ident = |e: &Json| -> Option<String> {
        e.pointer("/name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                e.pointer("/id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
    };
    // A root entity is identified by its own name/id; an entity
    // with ancestors is identified by its whole path, so two
    // entities sharing kind+name under different ancestors keep
    // distinct identities (and their key references still resolve,
    // since a `keyValue` carries the referent's full path too).
    let key = if path.len() == 1 {
        ident(last)?
    } else {
        let mut parts = Vec::with_capacity(path.len());
        for e in path {
            let ekind = e.pointer("/kind").and_then(|v| v.as_str())?;
            let eid = ident(e)?;
            parts.push(format!("{ekind}:{eid}"));
        }
        parts.join("/")
    };
    Some((kind, key))
}

fn decode(v: &Json) -> Prop {
    let get = |k: &str| v.pointer(&format!("/{k}"));
    if let Some(s) = get("stringValue").and_then(|v| v.as_str()) {
        return Prop::Scalar(Value::Str(s.to_string()));
    }
    if let Some(s) = get("integerValue").and_then(|v| v.as_str()) {
        return Prop::Scalar(s.parse().map(Value::Int).unwrap_or(Value::Null));
    }
    if let Some(f) = get("doubleValue").and_then(|v| v.as_f64()) {
        return Prop::Scalar(Value::Float(f));
    }
    if let Some(b) = get("booleanValue").and_then(|v| v.as_bool()) {
        return Prop::Scalar(Value::Bool(b));
    }
    if let Some(s) = get("timestampValue").and_then(|v| v.as_str()) {
        return Prop::Scalar(Value::Str(s.to_string()));
    }
    if let Some(k) = get("keyValue")
        && let Some((kind, name)) = decode_key(k)
    {
        return Prop::Key(kind, name);
    }
    if let Some(a) = get("arrayValue")
        .and_then(|v| v.pointer("/values"))
        .and_then(|v| v.as_array())
    {
        return Prop::Array(a.iter().map(decode).collect());
    }
    if let Some(e) = get("entityValue")
        .and_then(|v| v.pointer("/properties"))
        .and_then(|v| v.as_object())
    {
        return Prop::Entity(e.iter().map(|(k, v)| (k.clone(), decode(v))).collect());
    }
    Prop::Scalar(Value::Null)
}

#[allow(clippy::enum_variant_names)]
enum Kind {
    Root,
    KindDir { name: String },
    Entity { kind: String, key: String },
    Field { value: Prop },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A Datastore database, exposed as an arbor.
pub struct DatastoreAdapter {
    project: String,
    database: String,
    token: String,
    nodes: RefCell<Vec<Node>>,
    /// (kind, key) → properties.
    #[allow(clippy::type_complexity)]
    entities: RefCell<HashMap<(String, String), Vec<(String, Prop)>>>,
    /// (kind, key) → node.
    entity_nodes: RefCell<HashMap<(String, String), NodeId>>,
    /// kind → its directory node.
    kind_nodes: RefCell<HashMap<String, NodeId>>,
}

impl DatastoreAdapter {
    /// Connect to `datastore://PROJECT[/DATABASE][?account=EMAIL]`.
    pub fn connect(target: &str) -> Result<Self, DatastoreError> {
        let rest = target
            .strip_prefix("datastore://")
            .ok_or_else(|| DatastoreError::Target(target.to_string()))?;
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let (project, database) = match path.split_once('/') {
            Some((p, d)) => (p, d.to_string()),
            None => (path, String::new()),
        };
        if project.is_empty() {
            return Err(DatastoreError::Target(target.to_string()));
        }
        let account = query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("account=").map(str::to_string))
        });
        let token = gcp_token(account.as_deref()).map_err(DatastoreError::Api)?;
        let adapter = DatastoreAdapter {
            project: project.to_string(),
            database,
            token,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            entities: RefCell::new(HashMap::new()),
            entity_nodes: RefCell::new(HashMap::new()),
            kind_nodes: RefCell::new(HashMap::new()),
        };
        adapter.kinds()?; // probe
        Ok(adapter)
    }

    /// A human-readable locator: `/kind/key`.
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

    fn run_query(&self, query: Json) -> Result<Json, DatastoreError> {
        let url = format!(
            "https://datastore.googleapis.com/v1/projects/{}:runQuery",
            self.project
        );
        let mut body = json!({"query": query});
        if !self.database.is_empty() {
            body["databaseId"] = json!(self.database);
        }
        let routing = format!("project_id={}&database_id={}", self.project, self.database);
        ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("x-goog-request-params", &routing)
            .send_json(body)
            .map_err(|e| DatastoreError::Api(e.to_string()))?
            .into_json()
            .map_err(|e| DatastoreError::Api(format!("decoding response: {e}")))
    }

    /// The kind names, via a `__kind__` query.
    fn kinds(&self) -> Result<Vec<String>, DatastoreError> {
        let resp = self.run_query(json!({"kind": [{"name": "__kind__"}]}))?;
        if let Some(err) = resp.pointer("/error/message").and_then(|v| v.as_str()) {
            return Err(DatastoreError::Api(err.to_string()));
        }
        let mut out = Vec::new();
        if let Some(results) = resp
            .pointer("/batch/entityResults")
            .and_then(|v| v.as_array())
        {
            for r in results {
                if let Some((_, name)) = r.pointer("/entity/key").and_then(decode_key)
                    && !name.starts_with("__")
                {
                    out.push(name);
                }
            }
        }
        Ok(out)
    }

    fn push_node(&self, kind: Kind, name: Option<String>, parent: Option<NodeId>) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind,
            name,
            parent,
            children: RefCell::new(None),
        });
        id
    }

    /// A kind's entities via runQuery, following cursors.
    fn list_entities(&self, parent: NodeId, kind: &str) -> Vec<NodeId> {
        let mut ids = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = json!({"kind": [{"name": kind}]});
            if let Some(c) = &cursor {
                q["startCursor"] = json!(c);
            }
            let Ok(resp) = self.run_query(q) else { break };
            let batch = resp.pointer("/batch");
            if let Some(results) = batch
                .and_then(|b| b.pointer("/entityResults"))
                .and_then(|v| v.as_array())
            {
                for r in results {
                    let Some(entity) = r.pointer("/entity") else {
                        continue;
                    };
                    let Some((k, key)) = entity.pointer("/key").and_then(decode_key) else {
                        continue;
                    };
                    let props: Vec<(String, Prop)> = entity
                        .pointer("/properties")
                        .and_then(|v| v.as_object())
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
                        .unwrap_or_default();
                    self.entities
                        .borrow_mut()
                        .insert((k.clone(), key.clone()), props);
                    let id = self.push_node(
                        Kind::Entity {
                            kind: k.clone(),
                            key: key.clone(),
                        },
                        Some(key.clone()),
                        Some(parent),
                    );
                    self.entity_nodes.borrow_mut().insert((k, key), id);
                    ids.push(id);
                }
            }
            let more = batch
                .and_then(|b| b.pointer("/moreResults"))
                .and_then(|v| v.as_str())
                .unwrap_or("NO_MORE_RESULTS");
            if more != "NOT_FINISHED" {
                break;
            }
            cursor = batch
                .and_then(|b| b.pointer("/endCursor"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        ids
    }

    /// The node for `(kind, key)`, loading its kind if needed.
    fn entity_node(&self, kind: &str, key: &str) -> Option<NodeId> {
        let k = (kind.to_string(), key.to_string());
        if let Some(&id) = self.entity_nodes.borrow().get(&k) {
            return Some(id);
        }
        // Touch the kind's directory (loads all its entities).
        let dir = *self.kind_nodes.borrow().get(kind)?;
        self.children(dir);
        self.entity_nodes.borrow().get(&k).copied()
    }

    fn props_of(&self, node: NodeId) -> Option<Vec<(String, Prop)>> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { kind, key } => self
                .entities
                .borrow()
                .get(&(kind.clone(), key.clone()))
                .cloned(),
            _ => None,
        }
    }
}

impl AstAdapter for DatastoreAdapter {
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
            Root,
            KindDir(String),
            Entity,
            Field(Prop),
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::KindDir { name } => Plan::KindDir(name.clone()),
            Kind::Entity { .. } => Plan::Entity,
            Kind::Field { value } => Plan::Field(value.clone()),
        };
        let ids = match plan {
            Plan::Root => {
                let kinds = self.kinds().unwrap_or_default();
                kinds
                    .into_iter()
                    .map(|k| {
                        let id = self.push_node(
                            Kind::KindDir { name: k.clone() },
                            Some(k.clone()),
                            Some(node),
                        );
                        self.kind_nodes.borrow_mut().insert(k, id);
                        id
                    })
                    .collect()
            }
            Plan::KindDir(name) => self.list_entities(node, &name),
            Plan::Entity => {
                let props = self.props_of(node).unwrap_or_default();
                props
                    .iter()
                    .map(|(k, v)| {
                        self.push_node(
                            Kind::Field { value: v.clone() },
                            Some(k.clone()),
                            Some(node),
                        )
                    })
                    .collect()
            }
            Plan::Field(value) => match value {
                Prop::Array(items) => items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        self.push_node(
                            Kind::Field { value: v.clone() },
                            Some(i.to_string()),
                            Some(node),
                        )
                    })
                    .collect(),
                Prop::Entity(entries) => entries
                    .iter()
                    .map(|(k, v)| {
                        self.push_node(
                            Kind::Field { value: v.clone() },
                            Some(k.clone()),
                            Some(node),
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            },
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// `<kind>`, `<entity>`, `<key>`, `<array>`, `<map>`, or the
    /// scalar's JSON kind.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let t = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => return Vec::new(),
            Kind::KindDir { .. } => "kind",
            Kind::Entity { .. } => "entity",
            Kind::Field { value } => match value {
                Prop::Key(..) => "key",
                Prop::Array(_) => "array",
                Prop::Entity(_) => "map",
                Prop::Scalar(Value::Str(_)) => "string",
                Prop::Scalar(Value::Int(_) | Value::Float(_)) => "number",
                Prop::Scalar(Value::Bool(_)) => "boolean",
                Prop::Scalar(_) => "null",
            },
        };
        vec![t.to_string()]
    }

    /// An entity's scalar properties; a key property answers
    /// `kind/name`.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let props = self.props_of(node)?;
        match props.iter().find(|(k, _)| k == name)? {
            (_, Prop::Scalar(v)) => match v {
                Value::Null => None,
                other => Some(other.clone()),
            },
            (_, Prop::Key(kind, key)) => Some(Value::Str(format!("{kind}/{key}"))),
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field {
                value: Prop::Scalar(v),
            } => Some(v.clone()),
            Kind::Field {
                value: Prop::Key(kind, key),
            } => Some(Value::Str(format!("{kind}/{key}"))),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "n-entities" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::KindDir { .. } => Some(Value::Int(self.children(node).len() as i64)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `::prop~>` follows a native `keyValue` — no schema, no refs
    /// file; the key knows its kind.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let props = self.props_of(node)?;
        let (kind, key) = match props.iter().find(|(k, _)| k == property)? {
            (_, Prop::Key(kind, key)) => (kind.clone(), key.clone()),
            _ => return None,
        };
        // Make sure the kind directories exist.
        self.children(NodeId(0));
        self.entity_node(&kind, &key)
    }

    /// Every key property is an outgoing crosslink, labeled by the
    /// property name.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some(props) = self.props_of(node) else {
            return Vec::new();
        };
        self.children(NodeId(0));
        props
            .into_iter()
            .filter_map(|(k, v)| match v {
                Prop::Key(kind, key) => self.entity_node(&kind, &key).map(|n| (k, n)),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decode_key_root_entity_is_bare_name() {
        // A single-element path is unchanged: bare name/id.
        let by_name = json!({"path": [{"kind": "Task", "name": "todo"}]});
        assert_eq!(
            decode_key(&by_name),
            Some(("Task".to_string(), "todo".to_string()))
        );
        let by_id = json!({"path": [{"kind": "Task", "id": "42"}]});
        assert_eq!(
            decode_key(&by_id),
            Some(("Task".to_string(), "42".to_string()))
        );
    }

    #[test]
    fn decode_key_disambiguates_shared_ancestor_paths() {
        // Task("todo") under two different parents must not collide.
        let alice = json!({"path": [
            {"kind": "User", "name": "alice"},
            {"kind": "Task", "name": "todo"},
        ]});
        let bob = json!({"path": [
            {"kind": "User", "name": "bob"},
            {"kind": "Task", "name": "todo"},
        ]});
        let a = decode_key(&alice).expect("alice key decodes");
        let b = decode_key(&bob).expect("bob key decodes");
        // Both live under the same kind directory...
        assert_eq!(a.0, "Task");
        assert_eq!(b.0, "Task");
        // ...but hold distinct identities, so neither map entry
        // overwrites the other.
        assert_ne!(a.1, b.1);
    }
}
