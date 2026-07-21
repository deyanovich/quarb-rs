//! Kubernetes API adapter for the Quarb query engine.
//!
//! The Kubernetes REST API is already a tree, and the arbor
//! follows it: the root holds one listing per discovered
//! resource type — cluster-scoped kinds (`/nodes`) and the
//! all-namespaces view of namespaced kinds (`/pods`, kubectl's
//! `-A`) — while each namespace object holds its own scoped
//! listings (`/namespaces/prod/pods`), the way a Firestore
//! document holds subcollections. Objects descend as their JSON:
//! `/namespaces/prod/pods/web-1/spec/containers/0/image`.
//! RFC 3339 timestamp strings decode as instants, so
//! `[::created > 2026-01-01]` is a calendar comparison.
//!
//! **Owner references are native.** `metadata.ownerReferences`
//! is a typed pointer, so `::owner~>` walks a Pod to its
//! ReplicaSet to its Deployment with no schema, and `->owner` /
//! `->node` enumerate as labeled edges. `::app`-style lookups
//! fall through to `metadata.labels`, since labels are how
//! Kubernetes itself names things.
//!
//! Everything loads lazily: discovery at connect (the probe),
//! one list call per touched listing, single GETs for reference
//! targets. Read-only — the adapter only ever issues GETs.
//!
//! **Transport and auth**: the adapter shells out to
//! `kubectl get --raw` (git-adapter posture: plumbing over
//! subprocess, zero new dependencies), so kubeconfig contexts,
//! exec plugins, and cluster auth all behave exactly as kubectl
//! does. Target: `k8s:` (current context) or `k8s:CONTEXT`
//! (`kubernetes:` works too). `QUARB_KUBECTL` overrides the
//! binary — useful for wrappers like
//! `docker exec <container> kubectl`.

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// An error connecting to or reading a cluster.
#[derive(Debug, thiserror::Error)]
pub enum KubernetesError {
    #[error("kubectl: {0}")]
    Kubectl(String),
    #[error("kubernetes target: {0} (expected k8s: or k8s:CONTEXT)")]
    Target(String),
}

/// One discovered resource type.
#[derive(Clone)]
struct Resource {
    /// The plural REST name (`pods`).
    plural: String,
    /// The object kind (`Pod`).
    kind: String,
    /// The API group (empty for the core group).
    group: String,
    version: String,
    namespaced: bool,
    /// The listing's node name: the plural, or `plural.group`
    /// when two groups share a plural (kubectl's convention).
    node_name: String,
}

impl Resource {
    fn list_path(&self, ns: Option<&str>) -> String {
        let base = if self.group.is_empty() {
            format!("/api/{}", self.version)
        } else {
            format!("/apis/{}/{}", self.group, self.version)
        };
        match ns {
            Some(ns) => format!("{base}/namespaces/{ns}/{}", self.plural),
            None => format!("{base}/{}", self.plural),
        }
    }
}

/// Parse discovery documents into the resource table and the
/// kind → resource index (for owner resolution). `docs` is the
/// core `/api/v1` list followed by one list per group.
fn parse_discovery(docs: &[Json]) -> (Vec<Resource>, HashMap<String, usize>) {
    let mut resources: Vec<Resource> = Vec::new();
    let mut seen_plural: HashMap<String, usize> = HashMap::new();
    for doc in docs {
        // "v1" for the core group, "apps/v1" for the rest.
        let gv = doc
            .pointer("/groupVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("v1");
        let (group, version) = match gv.split_once('/') {
            Some((g, v)) => (g.to_string(), v.to_string()),
            None => (String::new(), gv.to_string()),
        };
        let Some(list) = doc.pointer("/resources").and_then(|v| v.as_array()) else {
            continue;
        };
        for r in list {
            let Some(plural) = r.pointer("/name").and_then(|v| v.as_str()) else {
                continue;
            };
            // Subresources (`pods/status`) are not listable trees.
            if plural.contains('/') {
                continue;
            }
            let listable = r
                .pointer("/verbs")
                .and_then(|v| v.as_array())
                .is_some_and(|vs| vs.iter().any(|v| v.as_str() == Some("list")));
            if !listable {
                continue;
            }
            let kind = r
                .pointer("/kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let namespaced = r
                .pointer("/namespaced")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let node_name = match seen_plural.get(plural.trim()) {
                // The core group is discovered first, so it keeps
                // the bare plural; a later group qualifies.
                Some(_) => format!("{plural}.{group}"),
                None => plural.to_string(),
            };
            seen_plural.entry(plural.to_string()).or_insert(0);
            resources.push(Resource {
                plural: plural.to_string(),
                kind,
                group: group.clone(),
                version: version.clone(),
                namespaced,
                node_name,
            });
        }
    }
    resources.sort_by(|a, b| a.node_name.cmp(&b.node_name));
    let mut kinds = HashMap::new();
    for (i, r) in resources.iter().enumerate() {
        if !r.kind.is_empty() {
            kinds.entry(r.kind.clone()).or_insert(i);
        }
    }
    (resources, kinds)
}

/// A decoded field value.
#[derive(Clone)]
enum Field {
    Scalar(Value),
    Map(Vec<(String, Field)>),
    Array(Vec<Field>),
}

fn decode(v: &Json) -> Field {
    match v {
        Json::Null => Field::Scalar(Value::Null),
        Json::Bool(b) => Field::Scalar(Value::Bool(*b)),
        Json::Number(n) => Field::Scalar(match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(0.0)),
        }),
        Json::String(s) => Field::Scalar(str_value(s)),
        Json::Array(a) => Field::Array(a.iter().map(decode).collect()),
        Json::Object(m) => Field::Map(m.iter().map(|(k, v)| (k.clone(), decode(v))).collect()),
    }
}

/// A string value; full RFC 3339 timestamps (the only date shape
/// the API emits) become instants. Bare dates stay strings — a
/// label value like `2026-01-01` is a name, not a moment.
fn str_value(s: &str) -> Value {
    if s.contains('T')
        && let Some((secs, nanos, offset_min)) = parse_iso(s)
    {
        return Value::Instant {
            secs,
            nanos,
            offset_min,
        };
    }
    Value::Str(s.to_string())
}

/// The quick facts extracted from an object's JSON.
struct ObjInfo {
    fields: Vec<(String, Field)>,
    name: String,
    namespace: Option<String>,
    kind: String,
    uid: Option<String>,
    created: Option<Value>,
    phase: Option<String>,
    /// `spec.nodeName` (pods).
    node_name: Option<String>,
    /// `metadata.ownerReferences`: (kind, name) pairs.
    owners: Vec<(String, String)>,
    /// `metadata.labels`.
    labels: Vec<(String, String)>,
}

/// `res` supplies the kind when the item omits it (list items
/// carry no `kind` of their own).
fn obj_info(o: &Json, res: &Resource) -> Option<ObjInfo> {
    let name = o.pointer("/metadata/name")?.as_str()?.to_string();
    let str_at = |p: &str| o.pointer(p).and_then(|v| v.as_str()).map(str::to_string);
    let fields = o
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        .unwrap_or_default();
    let owners = o
        .pointer("/metadata/ownerReferences")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some((
                        r.pointer("/kind")?.as_str()?.to_string(),
                        r.pointer("/name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let labels = o
        .pointer("/metadata/labels")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(ObjInfo {
        fields,
        name,
        namespace: str_at("/metadata/namespace"),
        kind: match str_at("/kind") {
            Some(k) => k,
            None => res.kind.clone(),
        },
        uid: str_at("/metadata/uid"),
        created: o
            .pointer("/metadata/creationTimestamp")
            .and_then(|v| v.as_str())
            .map(str_value)
            .filter(|v| matches!(v, Value::Instant { .. })),
        phase: str_at("/status/phase"),
        node_name: str_at("/spec/nodeName"),
        owners,
        labels,
    })
}

/// What a node is.
enum Kind {
    Root,
    /// A resource listing: at the root (cluster-scoped, or the
    /// all-namespaces view) or scoped under one namespace.
    Listing { res: usize, ns: Option<String> },
    /// An object; `key` indexes the objects table.
    Object { key: String },
    /// A field value inside an object.
    Field { value: Field },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A Kubernetes cluster, exposed as an arbor.
pub struct KubernetesAdapter {
    kubectl: String,
    context: Option<String>,
    resources: Vec<Resource>,
    /// kind name (`Pod`) → resource index.
    kinds: HashMap<String, usize>,
    nodes: RefCell<Vec<Node>>,
    objects: RefCell<HashMap<String, Rc<ObjInfo>>>,
    obj_nodes: RefCell<HashMap<String, NodeId>>,
    /// (resource, namespace) → listing node.
    listing_nodes: RefCell<HashMap<(usize, Option<String>), NodeId>>,
}

fn obj_key(res: &Resource, ns: Option<&str>, name: &str) -> String {
    format!(
        "{}.{}|{}|{}",
        res.plural,
        res.group,
        ns.unwrap_or("-"),
        name
    )
}

impl KubernetesAdapter {
    /// Connect to `k8s:` / `k8s:CONTEXT` (alias `kubernetes:`).
    pub fn connect(target: &str) -> Result<Self, KubernetesError> {
        let ctx = target
            .strip_prefix("k8s:")
            .or_else(|| target.strip_prefix("kubernetes:"))
            .ok_or_else(|| KubernetesError::Target(target.to_string()))?;
        let context = (!ctx.is_empty()).then(|| ctx.to_string());
        let kubectl =
            std::env::var("QUARB_KUBECTL").unwrap_or_else(|_| "kubectl".to_string());
        let mut adapter = KubernetesAdapter {
            kubectl,
            context,
            resources: Vec::new(),
            kinds: HashMap::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            objects: RefCell::new(HashMap::new()),
            obj_nodes: RefCell::new(HashMap::new()),
            listing_nodes: RefCell::new(HashMap::new()),
        };
        // Discovery is the connect probe: the core group, then
        // each group's preferred version.
        let mut docs = vec![adapter.call("/api/v1")?];
        if let Ok(groups) = adapter.call("/apis") {
            let gvs: Vec<String> = groups
                .pointer("/groups")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|g| {
                            g.pointer("/preferredVersion/groupVersion")?
                                .as_str()
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            for gv in gvs {
                if let Ok(doc) = adapter.call(&format!("/apis/{gv}")) {
                    docs.push(doc);
                }
            }
        }
        let (resources, kinds) = parse_discovery(&docs);
        if resources.is_empty() {
            return Err(KubernetesError::Kubectl(
                "discovery returned no listable resources".to_string(),
            ));
        }
        adapter.resources = resources;
        adapter.kinds = kinds;
        Ok(adapter)
    }

    /// A human-readable locator: the API-shaped path.
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

    fn call(&self, path: &str) -> Result<Json, KubernetesError> {
        let mut cmd = std::process::Command::new(&self.kubectl);
        if let Some(c) = &self.context {
            cmd.args(["--context", c]);
        }
        cmd.args(["get", "--raw", path]);
        let out = cmd
            .output()
            .map_err(|e| KubernetesError::Kubectl(format!("running {}: {e}", self.kubectl)))?;
        if !out.status.success() {
            return Err(KubernetesError::Kubectl(format!(
                "get --raw {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| KubernetesError::Kubectl(format!("decoding {path}: {e}")))
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

    fn intern_object(&self, info: ObjInfo, res: usize, parent: Option<NodeId>) -> NodeId {
        let r = &self.resources[res];
        let key = obj_key(r, info.namespace.as_deref(), &info.name);
        let name = info.name.clone();
        self.objects.borrow_mut().insert(key.clone(), Rc::new(info));
        let id = self.push_node(Kind::Object { key: key.clone() }, Some(name), parent);
        self.obj_nodes.borrow_mut().insert(key, id);
        id
    }

    /// List a resource (cluster, all-namespaces, or scoped).
    fn list_objects(&self, parent: NodeId, res: usize, ns: Option<&str>) -> Vec<NodeId> {
        let Ok(json) = self.call(&self.resources[res].list_path(ns)) else {
            // An unreadable listing is an empty one, per the
            // adapter's `children` contract (RBAC commonly denies
            // some resources; the rest of the tree still works).
            return Vec::new();
        };
        let Some(items) = json.pointer("/items").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|o| obj_info(o, &self.resources[res]))
            .map(|info| self.intern_object(info, res, Some(parent)))
            .collect()
    }

    /// The node for an object, fetching it singly when its
    /// listing was never touched. Interned under its listing
    /// node when one exists, so locators read the same whether
    /// an object arrived by listing or by reference.
    fn obj_node(&self, res: usize, ns: Option<&str>, name: &str) -> Option<NodeId> {
        let key = obj_key(&self.resources[res], ns, name);
        if let Some(&id) = self.obj_nodes.borrow().get(&key) {
            return Some(id);
        }
        let path = format!("{}/{name}", self.resources[res].list_path(ns));
        let json = self.call(&path).ok()?;
        let info = obj_info(&json, &self.resources[res])?;
        let parent = {
            let listings = self.listing_nodes.borrow();
            listings
                .get(&(res, ns.map(str::to_string)))
                .or_else(|| listings.get(&(res, None)))
                .copied()
        };
        Some(self.intern_object(info, res, parent))
    }

    /// Resolve an owner reference from `from`'s namespace.
    fn owner_node(&self, from: &ObjInfo, kind: &str, name: &str) -> Option<NodeId> {
        let res = *self.kinds.get(kind)?;
        let ns = if self.resources[res].namespaced {
            from.namespace.as_deref()
        } else {
            None
        };
        self.obj_node(res, ns, name)
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

    fn object(&self, key: &str) -> Option<Rc<ObjInfo>> {
        self.objects.borrow().get(key).cloned()
    }

    fn namespaces_res(&self) -> Option<usize> {
        self.kinds.get("Namespace").copied()
    }
}

impl AstAdapter for KubernetesAdapter {
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
            Listing(usize, Option<String>),
            Object(String),
            Field(Field),
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::Listing { res, ns } => Plan::Listing(*res, ns.clone()),
            Kind::Object { key } => Plan::Object(key.clone()),
            Kind::Field { value } => Plan::Field(value.clone()),
        };
        let ids = match plan {
            Plan::Root => (0..self.resources.len())
                .map(|res| {
                    let id = self.push_node(
                        Kind::Listing { res, ns: None },
                        Some(self.resources[res].node_name.clone()),
                        Some(node),
                    );
                    self.listing_nodes.borrow_mut().insert((res, None), id);
                    id
                })
                .collect(),
            Plan::Listing(res, ns) => self.list_objects(node, res, ns.as_deref()),
            Plan::Object(key) => {
                let Some(info) = self.object(&key) else {
                    return Vec::new();
                };
                let mut ids: Vec<NodeId> = info
                    .fields
                    .iter()
                    .map(|(k, v)| {
                        self.push_node(
                            Kind::Field { value: v.clone() },
                            Some(k.clone()),
                            Some(node),
                        )
                    })
                    .collect();
                // A namespace holds its scoped listings beside
                // its own fields, like a Firestore document's
                // subcollections.
                if info.kind == "Namespace" {
                    for res in 0..self.resources.len() {
                        if !self.resources[res].namespaced {
                            continue;
                        }
                        let ns = Some(info.name.clone());
                        let id = self.push_node(
                            Kind::Listing {
                                res,
                                ns: ns.clone(),
                            },
                            Some(self.resources[res].node_name.clone()),
                            Some(node),
                        );
                        self.listing_nodes.borrow_mut().insert((res, ns), id);
                        ids.push(id);
                    }
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

    /// `<resource>` on listings; the lowercased kind on objects
    /// (`<pod>`, `<replicaset>`); a field's value kind
    /// (`<instant>` for timestamps).
    fn traits(&self, node: NodeId) -> Vec<String> {
        let t = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => return Vec::new(),
            Kind::Listing { .. } => "resource".to_string(),
            Kind::Object { key } => match self.object(key) {
                Some(info) => info.kind.to_lowercase(),
                None => return Vec::new(),
            },
            Kind::Field { value } => match value {
                Field::Map(_) => "map",
                Field::Array(_) => "array",
                Field::Scalar(Value::Str(_)) => "string",
                Field::Scalar(Value::Int(_) | Value::Float(_)) => "number",
                Field::Scalar(Value::Bool(_)) => "boolean",
                Field::Scalar(Value::Instant { .. }) => "instant",
                Field::Scalar(_) => "null",
            }
            .to_string(),
        };
        vec![t]
    }

    /// `::name`, `::namespace`, `::kind`, `::uid`, `::created`,
    /// `::phase`, `::node`, `::owner`; anything else falls
    /// through to `metadata.labels` (`[::app = 'web']`).
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let key = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Object { key } => key.clone(),
            _ => return None,
        };
        let info = self.object(&key)?;
        match name {
            "name" => Some(Value::Str(info.name.clone())),
            "namespace" => info.namespace.clone().map(Value::Str),
            "kind" => Some(Value::Str(info.kind.clone())),
            "uid" => info.uid.clone().map(Value::Str),
            "created" => info.created.clone(),
            "phase" => info.phase.clone().map(Value::Str),
            "node" => info.node_name.clone().map(Value::Str),
            "owner" => info
                .owners
                .first()
                .map(|(k, n)| Value::Str(format!("{k}/{n}"))),
            other => info
                .labels
                .iter()
                .find(|(k, _)| k == other)
                .map(|(_, v)| Value::Str(v.clone())),
        }
    }

    /// A scalar field node projects to its value.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field {
                value: Field::Scalar(v),
            } => Some(v.clone()),
            _ => None,
        }
    }

    /// `;;;path`, `;;;n-fields` (an object's top-level field
    /// count), and `;;;length` (element count for an array or
    /// map, character count for a string).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "n-fields" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Object { key } => {
                    Some(Value::Int(self.object(key)?.fields.len() as i64))
                }
                _ => None,
            },
            "length" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Field { value: Field::Array(items) } => {
                    Some(Value::Int(items.len() as i64))
                }
                Kind::Field { value: Field::Map(entries) } => {
                    Some(Value::Int(entries.len() as i64))
                }
                Kind::Field {
                    value: Field::Scalar(Value::Str(s)),
                } => Some(Value::Int(s.chars().count() as i64)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `::owner~>` climbs the ownership chain; `::node~>` lands
    /// on the pod's Node; `::namespace~>` on the Namespace
    /// object.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let key = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Object { key } => key.clone(),
            _ => return None,
        };
        let info = self.object(&key)?;
        match property {
            "owner" => {
                let (kind, name) = info.owners.first()?.clone();
                self.owner_node(&info, &kind, &name)
            }
            "node" => {
                let name = info.node_name.clone()?;
                self.obj_node(*self.kinds.get("Node")?, None, &name)
            }
            "namespace" => {
                let ns = info.namespace.clone()?;
                self.obj_node(self.namespaces_res()?, None, &ns)
            }
            _ => None,
        }
    }

    /// Owner references and the pod → node placement, as labeled
    /// edges.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let key = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Object { key } => key.clone(),
            _ => return Vec::new(),
        };
        let Some(info) = self.object(&key) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (kind, name) in info.owners.clone() {
            if let Some(n) = self.owner_node(&info, &kind, &name) {
                out.push(("owner".to_string(), n));
            }
        }
        if let Some(name) = info.node_name.clone()
            && let Some(&res) = self.kinds.get("Node")
            && let Some(n) = self.obj_node(res, None, &name)
        {
            out.push(("node".to_string(), n));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamps_become_instants() {
        assert!(matches!(
            str_value("2026-07-20T12:00:00Z"),
            Value::Instant { .. }
        ));
        // Bare dates and date-like names stay strings.
        assert!(matches!(str_value("2026-01-01"), Value::Str(_)));
        assert!(matches!(str_value("v1.2.3"), Value::Str(_)));
        assert!(matches!(str_value("web-T1"), Value::Str(_)));
    }

    #[test]
    fn discovery_parses_and_qualifies_collisions() {
        let core = json!({"groupVersion": "v1", "resources": [
            {"name": "pods", "kind": "Pod", "namespaced": true,
             "verbs": ["get", "list", "watch"]},
            {"name": "pods/status", "kind": "Pod", "namespaced": true,
             "verbs": ["get"]},
            {"name": "events", "kind": "Event", "namespaced": true,
             "verbs": ["list"]},
            {"name": "bindings", "kind": "Binding", "namespaced": true,
             "verbs": ["create"]},
            {"name": "nodes", "kind": "Node", "namespaced": false,
             "verbs": ["list"]}]});
        let ev = json!({"groupVersion": "events.k8s.io/v1", "resources": [
            {"name": "events", "kind": "Event", "namespaced": true,
             "verbs": ["list"]}]});
        let (rs, kinds) = parse_discovery(&[core, ev]);
        let names: Vec<&str> = rs.iter().map(|r| r.node_name.as_str()).collect();
        // No subresource, no list-less binding; the group's
        // events qualified with its group.
        assert_eq!(names, ["events", "events.events.k8s.io", "nodes", "pods"]);
        assert_eq!(rs[kinds["Pod"]].plural, "pods");
        assert!(!rs[kinds["Node"]].namespaced);
        assert_eq!(
            rs[kinds["Pod"]].list_path(Some("prod")),
            "/api/v1/namespaces/prod/pods"
        );
        assert_eq!(rs[kinds["Node"]].list_path(None), "/api/v1/nodes");
    }

    #[test]
    fn obj_info_extracts_the_quick_facts() {
        let res = Resource {
            plural: "pods".into(),
            kind: "Pod".into(),
            group: String::new(),
            version: "v1".into(),
            namespaced: true,
            node_name: "pods".into(),
        };
        let o = json!({"metadata": {
            "name": "web-1", "namespace": "default", "uid": "u1",
            "creationTimestamp": "2026-07-01T00:00:00Z",
            "labels": {"app": "web"},
            "ownerReferences": [{"kind": "ReplicaSet", "name": "web-rs"}]},
            "spec": {"nodeName": "worker-1"},
            "status": {"phase": "Running"}});
        let info = obj_info(&o, &res).unwrap();
        assert_eq!(info.name, "web-1");
        assert_eq!(info.kind, "Pod"); // filled from the resource
        assert_eq!(info.phase.as_deref(), Some("Running"));
        assert_eq!(info.node_name.as_deref(), Some("worker-1"));
        assert_eq!(info.owners, [("ReplicaSet".to_string(), "web-rs".to_string())]);
        assert_eq!(info.labels, [("app".to_string(), "web".to_string())]);
        assert!(matches!(info.created, Some(Value::Instant { .. })));
    }

    #[test]
    fn target_needs_the_scheme() {
        assert!(matches!(
            KubernetesAdapter::connect("kube:ctx"),
            Err(KubernetesError::Target(_))
        ));
    }
}
