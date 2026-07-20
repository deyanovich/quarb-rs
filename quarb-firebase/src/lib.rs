//! Firebase Realtime Database adapter for the Quarb query engine.
//!
//! An RTDB *is* a JSON tree, so the mapping is `quarb-json`'s —
//! objects' fields (and arrays' elements) as named children,
//! scalars carrying the value, traits and `;;;type` naming the
//! JSON kind — but the tree lives on the other end of a REST API
//! and can be enormous (the public Hacker News database has tens
//! of millions of nodes), so nothing is ever fetched whole.
//!
//! **Loading model: everything lazy, one node at a time.** A node
//! materializes on first touch with a `?shallow=true` GET — a
//! scalar arrives as its value, a container as its key set — and
//! is cached for the adapter's lifetime. Two consequences to
//! respect: each newly-touched node is one HTTP request, and
//! *unanchored descent* (`//name`) over a large database walks
//! everything it touches — keep queries anchored the way you'd
//! keep BigQuery queries off `SELECT *`.
//!
//! **Properties are direct fetches.** `::field` on a node GETs
//! `path/field.json` — the cheapest possible request (one
//! scalar, no shallow walk of siblings). This is a deliberate
//! ergonomic divergence from `quarb-json` (where fields are only
//! child hops): on a remote tree, `/items/42::score` should cost
//! one tiny request, and does.
//!
//! **References resolve by hint.** RTDB has no schema, so `~>`
//! always takes a hint naming a root-relative container:
//! `::parent~>item` reads the `parent` field and lands on
//! `<base>/item/<value>` — the same convention as the relational
//! adapters' hint (the target "table"). Chains work.
//!
//! **Target syntax**: `firebase://HOST/BASE/PATH[?QUERY]` — e.g.
//! `firebase://hacker-news.firebaseio.com/v0`. HTTPS is assumed.
//! Anything in the query string is appended to every request:
//! `?auth=SECRET` (legacy tokens) or `?access_token=TOKEN`
//! (OAuth2) authenticate private databases; public ones need
//! nothing.
//!
//! **Declared references.** The database holds no schema, so the
//! reference schema can be supplied client-side: a *refs*
//! document mapping field names to root-relative target
//! containers —
//! `{"refs": {"parent": "item", "by": "user", "kids/*": "item"}}`
//! (the `field/*` form declares an array field whose *elements*
//! reference the target). With refs supplied, bare `~>` resolves
//! (`::parent~>`), and `->` crosslinks enumerate: every declared
//! field with a value becomes a labeled, probed edge, including
//! one edge per element for array fields. An inline hint always
//! overrides. Reverse resolution (`<~`) stays empty: it would
//! require scanning the referrer container, which is exactly what
//! opaque containers refuse (a server-side `.indexOn` query is
//! the recorded v2 path).
//!
//! The adapter only ever GETs; the language stays read-only.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a database.
#[derive(Debug, thiserror::Error)]
pub enum FirebaseError {
    #[error("firebase: {0}")]
    Http(#[from] Box<ureq::Error>),
    #[error("firebase: {0}")]
    Api(String),
    #[error("firebase target: {0} (expected firebase://HOST/BASE/PATH[?QUERY])")]
    Target(String),
}

/// What a fetched node turned out to be.
enum Kind {
    /// A scalar (string, number, boolean) — or JSON null.
    Scalar(Value),
    /// A container: children in deterministic order (numeric keys
    /// numerically, then the rest lexically — array-ish nodes read
    /// in element order).
    Container(Vec<NodeId>),
    /// A container that refuses enumeration (permission-scoped or
    /// unbounded — the database answered the shallow GET with a
    /// 401). Its children are reachable by *name* only: `/item/1`
    /// navigates, `/item/*` is empty. This is the honest shape of
    /// databases like the public Hacker News API, whose `/v0/item`
    /// holds tens of millions of keys.
    Opaque,
}

struct Node {
    /// The RTDB path below the base, `""` for the root.
    path: String,
    name: Option<String>,
    parent: Option<NodeId>,
    /// `None` until first touch.
    kind: RefCell<Option<Kind>>,
}

/// A failed GET, classified so the caller can tell a
/// permission-denied container (an exact `401`) from a real
/// error — and so the request URL, which carries the `?auth=…`
/// secret, never enters the message.
enum GetError {
    /// A non-2xx HTTP status.
    Status(u16),
    /// A transport or decode failure. Built from ureq's error
    /// *kind* (and its higher-level detail: host, scheme), never
    /// its URL — so `?auth=…` / `access_token=…` cannot leak.
    Other(String),
}

impl std::fmt::Display for GetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GetError::Status(code) => write!(f, "status code {code}"),
            GetError::Other(msg) => f.write_str(msg),
        }
    }
}

/// A declared reference schema: field name (or `field/*` for
/// array elements) → root-relative target container.
pub type Refs = std::collections::HashMap<String, String>;

/// Parse a refs document: `{"refs": {"parent": "item", ...}}`.
pub fn parse_refs(text: &str) -> Result<Refs, FirebaseError> {
    let json: Json = serde_json::from_str(text)
        .map_err(|e| FirebaseError::Api(format!("refs document: {e}")))?;
    let map = json
        .get("refs")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            FirebaseError::Api(
                "refs document: expected {\"refs\": {\"field\": \"container\"}}".into(),
            )
        })?;
    map.iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|t| (k.clone(), t.to_string()))
                .ok_or_else(|| {
                    FirebaseError::Api(format!("refs document: '{k}' target must be a string"))
                })
        })
        .collect()
}

/// A Firebase Realtime Database (subtree), exposed as an arbor.
pub struct FirebaseAdapter {
    /// `https://HOST/BASE/PATH` (no trailing slash).
    base: String,
    /// The query string to append to every request (auth).
    query: String,
    nodes: RefCell<Vec<Node>>,
    /// path → node, so re-discovered nodes intern to the same id.
    by_path: RefCell<HashMap<String, NodeId>>,
    /// The declared reference schema (client-side; the database
    /// has none).
    refs: Refs,
}

impl FirebaseAdapter {
    /// Connect to `firebase://HOST/BASE/PATH[?QUERY]`. Verifies
    /// the target answers (one shallow GET of the root).
    pub fn connect(target: &str) -> Result<Self, FirebaseError> {
        Self::connect_with_refs(target, Refs::new())
    }

    /// [`connect`], with a declared reference schema (see the
    /// module doc): bare `~>` and `->` crosslinks work for the
    /// declared fields.
    pub fn connect_with_refs(target: &str, refs: Refs) -> Result<Self, FirebaseError> {
        let rest = target
            .strip_prefix("firebase://")
            .ok_or_else(|| FirebaseError::Target(target.to_string()))?;
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, q.to_string()),
            None => (rest, String::new()),
        };
        if path.is_empty() {
            return Err(FirebaseError::Target(target.to_string()));
        }
        let adapter = FirebaseAdapter {
            base: format!("https://{}", path.trim_end_matches('/')),
            query,
            nodes: RefCell::new(vec![Node {
                path: String::new(),
                name: None,
                parent: None,
                kind: RefCell::new(None),
            }]),
            by_path: RefCell::new(HashMap::new()),
            refs,
        };
        // Touch the root: transport errors surface here, not
        // mid-query. A 401 is not fatal — it marks the root
        // opaque (readable by name, not enumerable), which is how
        // permission-scoped databases answer.
        adapter
            .fetch(NodeId(0))
            .map_err(|e| FirebaseError::Api(format!("probing the database root: {e}")))?;
        Ok(adapter)
    }

    /// A human-readable locator: the RTDB path.
    pub fn locator(&self, node: NodeId) -> String {
        let path = &self.nodes.borrow()[node.0 as usize].path;
        if path.is_empty() {
            "/".to_string()
        } else {
            format!("/{path}")
        }
    }

    fn url(&self, path: &str, shallow: bool) -> String {
        let mut url = if path.is_empty() {
            format!("{}.json", self.base)
        } else {
            format!("{}/{path}.json", self.base)
        };
        let mut params = Vec::new();
        if shallow {
            params.push("shallow=true".to_string());
        }
        if !self.query.is_empty() {
            params.push(self.query.clone());
        }
        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }
        url
    }

    fn get(&self, url: &str) -> Result<Json, GetError> {
        // Never stringify the ureq error itself: its `Display`
        // splices the full request URL — auth secret and all —
        // into the message. Classify structurally instead.
        let resp = match ureq::get(url).call() {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) => return Err(GetError::Status(code)),
            Err(ureq::Error::Transport(t)) => {
                let mut msg = t.kind().to_string();
                if let Some(detail) = t.message() {
                    msg.push_str(": ");
                    msg.push_str(detail);
                }
                return Err(GetError::Other(msg));
            }
        };
        resp.into_json()
            .map_err(|e| GetError::Other(format!("decoding response: {e}")))
    }

    /// Intern a child node under `parent`.
    fn intern(&self, parent: NodeId, key: &str) -> NodeId {
        let path = {
            let nodes = self.nodes.borrow();
            let ppath = &nodes[parent.0 as usize].path;
            if ppath.is_empty() {
                key.to_string()
            } else {
                format!("{ppath}/{key}")
            }
        };
        if let Some(&id) = self.by_path.borrow().get(&path) {
            return id;
        }
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            path: path.clone(),
            name: Some(key.to_string()),
            parent: Some(parent),
            kind: RefCell::new(None),
        });
        self.by_path.borrow_mut().insert(path, id);
        id
    }

    /// Materialize a node on first touch: one shallow GET. Errors
    /// degrade to an empty container with a warning on stderr (the
    /// adapter trait has no error channel mid-navigation).
    fn fetch(&self, node: NodeId) -> Result<(), String> {
        let (path, fetched) = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            (n.path.clone(), n.kind.borrow().is_some())
        };
        if fetched {
            return Ok(());
        }
        let json = match self.get(&self.url(&path, true)) {
            Ok(j) => j,
            // Permission-scoped or unbounded containers answer
            // enumeration with a 401: mark opaque, keep navigating
            // by name. Match the status structurally — a substring
            // test misfired on any error (e.g. a 500) whose URL or
            // message merely contained "401".
            Err(GetError::Status(401)) => {
                *self.nodes.borrow()[node.0 as usize].kind.borrow_mut() = Some(Kind::Opaque);
                return Ok(());
            }
            Err(e) => return Err(e.to_string()),
        };
        let kind = match &json {
            Json::Object(map) => {
                // Deterministic order: numeric keys numerically
                // (array-ish nodes in element order), the rest
                // lexically after them.
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_by(|a, b| match (a.parse::<i64>(), b.parse::<i64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => a.cmp(b),
                });
                let children = keys.iter().map(|k| self.intern(node, k)).collect();
                Kind::Container(children)
            }
            Json::Array(items) => {
                let children = (0..items.len())
                    .map(|i| self.intern(node, &i.to_string()))
                    .collect();
                Kind::Container(children)
            }
            other => Kind::Scalar(scalar_of(other)),
        };
        *self.nodes.borrow()[node.0 as usize].kind.borrow_mut() = Some(kind);
        Ok(())
    }

    fn touched(&self, node: NodeId) {
        if let Err(e) = self.fetch(node) {
            let path = self.locator(node);
            eprintln!("quarb-firebase: fetching {path}: {e}");
            *self.nodes.borrow()[node.0 as usize].kind.borrow_mut() =
                Some(Kind::Container(Vec::new()));
        }
    }

    /// One field of a node, as a single direct GET (cached as an
    /// interned child).
    fn field(&self, node: NodeId, name: &str) -> Option<Value> {
        let child = self.intern(node, name);
        self.touched(child);
        let nodes = self.nodes.borrow();
        match &*nodes[child.0 as usize].kind.borrow() {
            Some(Kind::Scalar(v)) => match v {
                Value::Null => None,
                other => Some(other.clone()),
            },
            _ => None,
        }
    }
}

/// The scalar value of a JSON primitive.
fn scalar_of(value: &Json) -> Value {
    match value {
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        Json::String(s) => Value::Str(s.clone()),
        _ => Value::Null,
    }
}

impl AstAdapter for FirebaseAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.touched(node);
        let nodes = self.nodes.borrow();
        match &*nodes[node.0 as usize].kind.borrow() {
            Some(Kind::Container(c)) => c.clone(),
            _ => Vec::new(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// Name-addressed navigation — the path through opaque
    /// containers (one direct GET; no enumeration). Enumerated
    /// containers answer from their key set without a request.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.touched(node);
        {
            let nodes = self.nodes.borrow();
            match &*nodes[node.0 as usize].kind.borrow() {
                Some(Kind::Scalar(_)) => return Vec::new(),
                Some(Kind::Container(c)) => {
                    return c
                        .iter()
                        .copied()
                        .filter(|&c| nodes[c.0 as usize].name.as_deref() == Some(name))
                        .collect();
                }
                _ => {}
            }
        }
        // Opaque: probe the named child directly.
        let child = self.intern(node, name);
        self.touched(child);
        let nodes = self.nodes.borrow();
        match &*nodes[child.0 as usize].kind.borrow() {
            Some(Kind::Scalar(Value::Null)) | None => Vec::new(),
            _ => vec![child],
        }
    }

    /// The JSON kind, once known: `<object>` / `<string>` /
    /// `<number>` / `<boolean>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.touched(node);
        let nodes = self.nodes.borrow();
        let t = match &*nodes[node.0 as usize].kind.borrow() {
            Some(Kind::Container(_) | Kind::Opaque) => "object",
            Some(Kind::Scalar(Value::Str(_))) => "string",
            Some(Kind::Scalar(Value::Int(_) | Value::Float(_))) => "number",
            Some(Kind::Scalar(Value::Bool(_))) => "boolean",
            _ => "null",
        };
        vec![t.to_string()]
    }

    /// `::field` — one direct GET of `path/field.json`.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.field(node, name)
    }

    /// A scalar projects to its value; a container has no default
    /// projection.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.touched(node);
        let nodes = self.nodes.borrow();
        match &*nodes[node.0 as usize].kind.borrow() {
            Some(Kind::Scalar(v)) => Some(v.clone()),
            _ => None,
        }
    }

    /// `;;;type`, `;;;length` (children of a container), and
    /// `;;;path` (the RTDB path — the node's address in the
    /// database).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "type" => Some(Value::Str(self.traits(node).remove(0))),
            "length" => {
                self.touched(node);
                let nodes = self.nodes.borrow();
                match &*nodes[node.0 as usize].kind.borrow() {
                    Some(Kind::Container(c)) => Some(Value::Int(c.len() as i64)),
                    Some(Kind::Scalar(Value::Str(s))) => Some(Value::Int(s.chars().count() as i64)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Hint-based only (RTDB has no schema): `::parent~>item`
    /// reads the `parent` field and lands on `<base>/item/<value>`
    /// — the hint names a root-relative container, like the
    /// relational adapters' target table.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let container = hint.or_else(|| self.refs.get(property).map(String::as_str))?;
        let value = self.field(node, property)?;
        let root_child = self.intern(NodeId(0), container);
        let target = self.intern(root_child, &value.to_string());
        self.touched(target);
        let nodes = self.nodes.borrow();
        match &*nodes[target.0 as usize].kind.borrow() {
            Some(Kind::Scalar(Value::Null)) | None => None,
            _ => Some(target),
        }
    }

    /// Every declared field with a value is an outgoing crosslink,
    /// labeled by the field name; a `field/*` declaration yields
    /// one edge per array element. Each edge is probed (one GET)
    /// so dangling references stay out.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let mut declared: Vec<(&String, &String)> = self.refs.iter().collect();
        declared.sort();
        let mut out = Vec::new();
        for (field, target) in declared {
            let root_child = self.intern(NodeId(0), target);
            if let Some(elem_field) = field.strip_suffix("/*") {
                let container = self.intern(node, elem_field);
                for elem in self.children(container) {
                    let Some(v) = self.default_value(elem) else {
                        continue;
                    };
                    let t = self.intern(root_child, &v.to_string());
                    self.touched(t);
                    let nodes = self.nodes.borrow();
                    if !matches!(
                        &*nodes[t.0 as usize].kind.borrow(),
                        Some(Kind::Scalar(Value::Null)) | None
                    ) {
                        out.push((elem_field.to_string(), t));
                    }
                }
            } else if let Some(v) = self.field(node, field) {
                let t = self.intern(root_child, &v.to_string());
                self.touched(t);
                let nodes = self.nodes.borrow();
                if !matches!(
                    &*nodes[t.0 as usize].kind.borrow(),
                    Some(Kind::Scalar(Value::Null)) | None
                ) {
                    out.push((field.clone(), t));
                }
            }
        }
        out
    }
}
