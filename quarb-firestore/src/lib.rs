//! Google Firestore (native mode) adapter for the Quarb query
//! engine.
//!
//! Firestore alternates collections and documents: the database
//! root holds collections, a collection holds documents, and a
//! document holds typed fields *and* subcollections. The arbor
//! follows: `/tracks/mars::title`, with map and array fields
//! descending as child nodes and subcollections appearing beside
//! them.
//!
//! **References are native.** Firestore's `referenceValue` is a
//! typed document pointer, which makes this the first remote
//! adapter where `~>` needs neither a schema nor a declared refs
//! file: `::album~>::title` follows the field's own target, and
//! `->` enumerates every reference field as a labeled edge.
//!
//! Everything loads lazily — collection ids by `listCollectionIds`,
//! documents by paginated list, one level on first touch, cached.
//! Reads bill per document, so the lazy model is the cost model.
//! The adapter only ever GETs/POSTs list calls; it never writes.
//!
//! **Auth and target**: `firestore://PROJECT/DATABASE[?account=EMAIL]`
//! (`(default)` for the unnamed database). A bearer token comes
//! from `QUARB_GCP_TOKEN`, else `gcloud auth print-access-token
//! [account]`. Named databases need the `x-goog-request-params`
//! routing header; the adapter sends it always.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a database.
#[derive(Debug, thiserror::Error)]
pub enum FirestoreError {
    #[error("firestore: {0}")]
    Api(String),
    #[error("firestore target: {0} (expected firestore://PROJECT/DATABASE[?account=EMAIL])")]
    Target(String),
}

/// A bearer token: `QUARB_GCP_TOKEN`, else gcloud.
pub(crate) fn gcp_token(account: Option<&str>) -> Result<String, String> {
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

/// What a node is.
enum Kind {
    Root,
    /// A collection (or subcollection): `path` relative to the
    /// documents root, e.g. `tracks` or `tracks/mars/notes`.
    Collection {
        path: String,
    },
    /// A document: relative `path` plus its decoded fields.
    Doc {
        path: String,
    },
    /// A field value inside a document (scalar, map, or array
    /// element).
    Field {
        value: Field,
    },
}

/// A decoded Firestore value.
#[derive(Clone)]
enum Field {
    Scalar(Value),
    /// A document reference: the target's path relative to the
    /// documents root.
    Reference(String),
    Map(Vec<(String, Field)>),
    Array(Vec<Field>),
}

fn decode(v: &Json) -> Field {
    let obj = v.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    if let Some(s) = get("stringValue").and_then(|v| v.as_str()) {
        return Field::Scalar(Value::Str(s.to_string()));
    }
    if let Some(s) = get("integerValue").and_then(|v| v.as_str()) {
        return Field::Scalar(s.parse().map(Value::Int).unwrap_or(Value::Null));
    }
    if let Some(f) = get("doubleValue").and_then(|v| v.as_f64()) {
        return Field::Scalar(Value::Float(f));
    }
    if let Some(b) = get("booleanValue").and_then(|v| v.as_bool()) {
        return Field::Scalar(Value::Bool(b));
    }
    if let Some(s) = get("timestampValue").and_then(|v| v.as_str()) {
        return Field::Scalar(Value::Str(s.to_string()));
    }
    if let Some(r) = get("referenceValue").and_then(|v| v.as_str()) {
        // ".../databases/DB/documents/coll/doc" → "coll/doc"
        let rel = r
            .split_once("/documents/")
            .map(|(_, p)| p.to_string())
            .unwrap_or_else(|| r.to_string());
        return Field::Reference(rel);
    }
    if let Some(m) = get("mapValue")
        .and_then(|v| v.pointer("/fields"))
        .and_then(|v| v.as_object())
    {
        return Field::Map(m.iter().map(|(k, v)| (k.clone(), decode(v))).collect());
    }
    if let Some(a) = get("arrayValue")
        .and_then(|v| v.pointer("/values"))
        .and_then(|v| v.as_array())
    {
        return Field::Array(a.iter().map(decode).collect());
    }
    Field::Scalar(Value::Null)
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A Firestore database, exposed as an arbor.
pub struct FirestoreAdapter {
    project: String,
    database: String,
    token: String,
    nodes: RefCell<Vec<Node>>,
    /// document path → decoded fields (fetched with the listing).
    docs: RefCell<HashMap<String, Vec<(String, Field)>>>,
    /// document path → node (for reference resolution).
    doc_nodes: RefCell<HashMap<String, NodeId>>,
}

impl FirestoreAdapter {
    /// Connect to `firestore://PROJECT/DATABASE[?account=EMAIL]`.
    pub fn connect(target: &str) -> Result<Self, FirestoreError> {
        let rest = target
            .strip_prefix("firestore://")
            .ok_or_else(|| FirestoreError::Target(target.to_string()))?;
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let (project, database) = path
            .split_once('/')
            .ok_or_else(|| FirestoreError::Target(target.to_string()))?;
        let account = query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("account=").map(str::to_string))
        });
        let token = gcp_token(account.as_deref()).map_err(FirestoreError::Api)?;
        let adapter = FirestoreAdapter {
            project: project.to_string(),
            database: database.to_string(),
            token,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            docs: RefCell::new(HashMap::new()),
            doc_nodes: RefCell::new(HashMap::new()),
        };
        // Probe (connection errors surface here, not mid-query).
        adapter.list_collections("")?;
        Ok(adapter)
    }

    /// A human-readable locator: the document/field path.
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

    fn base(&self) -> String {
        format!(
            "https://firestore.googleapis.com/v1/projects/{}/databases/{}/documents",
            self.project, self.database
        )
    }

    fn call(&self, method: &str, url: &str, body: Option<Json>) -> Result<Json, FirestoreError> {
        let routing = format!("project_id={}&database_id={}", self.project, self.database);
        let req = ureq::request(method, url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("x-goog-request-params", &routing);
        let resp = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        }
        .map_err(|e| FirestoreError::Api(e.to_string()))?;
        resp.into_json()
            .map_err(|e| FirestoreError::Api(format!("decoding response: {e}")))
    }

    /// The collection ids under a document path (`""` = the root).
    fn list_collections(&self, doc_path: &str) -> Result<Vec<String>, FirestoreError> {
        let url = if doc_path.is_empty() {
            format!("{}:listCollectionIds", self.base())
        } else {
            format!("{}/{}:listCollectionIds", self.base(), doc_path)
        };
        let resp = self.call("POST", &url, Some(serde_json::json!({})))?;
        Ok(resp
            .pointer("/collectionIds")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
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

    /// List a collection's documents (paginated), caching fields.
    fn list_docs(&self, parent: NodeId, coll_path: &str) -> Vec<NodeId> {
        let mut ids = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = format!("{}/{}?pageSize=300", self.base(), coll_path);
            if let Some(p) = &page {
                url.push_str(&format!("&pageToken={p}"));
            }
            let resp = match self.call("GET", &url, None) {
                Ok(resp) => resp,
                // A failure on a *continuation* request — we already
                // fetched at least one page, so `page` holds the next
                // token — truncates the listing. `children` has no
                // error channel, so swallowing this reported a
                // silently-short collection as if complete (a wrong
                // count) and cached it for the adapter's lifetime.
                // Fail loud rather than return a partial answer.
                Err(e) if page.is_some() => {
                    panic!("firestore: listing {coll_path} truncated mid-pagination: {e}");
                }
                // First request unreadable: an empty (unreadable)
                // collection, per the adapter's `children` contract.
                Err(_) => break,
            };
            if let Some(docs) = resp.pointer("/documents").and_then(|v| v.as_array()) {
                for d in docs {
                    let Some(full) = d.pointer("/name").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let rel = full
                        .split_once("/documents/")
                        .map(|(_, p)| p.to_string())
                        .unwrap_or_default();
                    let doc_id = rel.rsplit('/').next().unwrap_or("").to_string();
                    let fields: Vec<(String, Field)> = d
                        .pointer("/fields")
                        .and_then(|v| v.as_object())
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
                        .unwrap_or_default();
                    self.docs.borrow_mut().insert(rel.clone(), fields);
                    let id =
                        self.push_node(Kind::Doc { path: rel.clone() }, Some(doc_id), Some(parent));
                    self.doc_nodes.borrow_mut().insert(rel, id);
                    ids.push(id);
                }
            }
            page = resp
                .pointer("/nextPageToken")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if page.is_none() {
                break;
            }
        }
        ids
    }

    /// The node for a document path, fetching the document if it
    /// was not already listed.
    fn doc_node(&self, path: &str) -> Option<NodeId> {
        if let Some(&id) = self.doc_nodes.borrow().get(path) {
            return Some(id);
        }
        // Fetch the single document; its parent chain is interned
        // shallowly (collection under root).
        let url = format!("{}/{}", self.base(), path);
        let d = self.call("GET", &url, None).ok()?;
        d.pointer("/name")?;
        let fields: Vec<(String, Field)> = d
            .pointer("/fields")
            .and_then(|v| v.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
            .unwrap_or_default();
        self.docs.borrow_mut().insert(path.to_string(), fields);
        let doc_id = path.rsplit('/').next().unwrap_or("").to_string();
        let id = self.push_node(
            Kind::Doc {
                path: path.to_string(),
            },
            Some(doc_id),
            None,
        );
        self.doc_nodes.borrow_mut().insert(path.to_string(), id);
        Some(id)
    }

    fn field_children(&self, parent: NodeId, f: &Field) -> Vec<NodeId> {
        match f {
            Field::Map(entries) => entries
                .iter()
                .map(|(k, v)| {
                    self.push_node(
                        Kind::Field { value: v.clone() },
                        Some(k.clone()),
                        Some(parent),
                    )
                })
                .collect(),
            Field::Array(items) => items
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    self.push_node(
                        Kind::Field { value: v.clone() },
                        Some(i.to_string()),
                        Some(parent),
                    )
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl AstAdapter for FirestoreAdapter {
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
        // Extract what we need before pushing nodes (which
        // borrows mutably).
        enum Plan {
            Root,
            Coll(String),
            Doc(String),
            Field(Field),
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::Collection { path } => Plan::Coll(path.clone()),
            Kind::Doc { path } => Plan::Doc(path.clone()),
            Kind::Field { value } => Plan::Field(value.clone()),
        };
        let ids = match plan {
            Plan::Root => {
                let colls = self.list_collections("").unwrap_or_default();
                colls
                    .into_iter()
                    .map(|c| {
                        self.push_node(Kind::Collection { path: c.clone() }, Some(c), Some(node))
                    })
                    .collect()
            }
            Plan::Coll(path) => self.list_docs(node, &path),
            Plan::Doc(path) => {
                // Fields first, then subcollections.
                let fields = self.docs.borrow().get(&path).cloned().unwrap_or_default();
                let mut ids: Vec<NodeId> = fields
                    .iter()
                    .map(|(k, v)| {
                        self.push_node(
                            Kind::Field { value: v.clone() },
                            Some(k.clone()),
                            Some(node),
                        )
                    })
                    .collect();
                for c in self.list_collections(&path).unwrap_or_default() {
                    ids.push(self.push_node(
                        Kind::Collection {
                            path: format!("{path}/{c}"),
                        },
                        Some(c),
                        Some(node),
                    ));
                }
                ids
            }
            Plan::Field(value) => self.field_children(node, &value),
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

    /// `<collection>`, `<document>`, `<map>`, `<array>`,
    /// `<reference>`, or the scalar's JSON kind.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let t = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => return Vec::new(),
            Kind::Collection { .. } => "collection",
            Kind::Doc { .. } => "document",
            Kind::Field { value } => match value {
                Field::Map(_) => "map",
                Field::Array(_) => "array",
                Field::Reference(_) => "reference",
                Field::Scalar(Value::Str(_)) => "string",
                Field::Scalar(Value::Int(_) | Value::Float(_)) => "number",
                Field::Scalar(Value::Bool(_)) => "boolean",
                Field::Scalar(_) => "null",
            },
        };
        vec![t.to_string()]
    }

    /// A document's scalar fields (`::title`); a reference field
    /// answers its target path.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { path } => path.clone(),
            _ => return None,
        };
        let docs = self.docs.borrow();
        let fields = docs.get(&path)?;
        match fields.iter().find(|(k, _)| k == name)? {
            (_, Field::Scalar(v)) => match v {
                Value::Null => None,
                other => Some(other.clone()),
            },
            (_, Field::Reference(p)) => Some(Value::Str(p.clone())),
            _ => None,
        }
    }

    /// A scalar field node projects to its value; a reference to
    /// its target path.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field {
                value: Field::Scalar(v),
            } => Some(v.clone()),
            Kind::Field {
                value: Field::Reference(p),
            } => Some(Value::Str(p.clone())),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "n-fields" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Doc { path } => Some(Value::Int(self.docs.borrow().get(path)?.len() as i64)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `::field~>` follows a native `referenceValue` — no schema,
    /// no refs file; the field knows its target.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { path } => path.clone(),
            _ => return None,
        };
        let target = {
            let docs = self.docs.borrow();
            let fields = docs.get(&path)?;
            match fields.iter().find(|(k, _)| k == property)? {
                (_, Field::Reference(p)) => p.clone(),
                _ => return None,
            }
        };
        self.doc_node(&target)
    }

    /// Every reference field is an outgoing crosslink, labeled by
    /// the field name.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { path } => path.clone(),
            _ => return Vec::new(),
        };
        let refs: Vec<(String, String)> = {
            let docs = self.docs.borrow();
            let Some(fields) = docs.get(&path) else {
                return Vec::new();
            };
            fields
                .iter()
                .filter_map(|(k, v)| match v {
                    Field::Reference(p) => Some((k.clone(), p.clone())),
                    _ => None,
                })
                .collect()
        };
        refs.into_iter()
            .filter_map(|(k, p)| self.doc_node(&p).map(|n| (k, n)))
            .collect()
    }
}
