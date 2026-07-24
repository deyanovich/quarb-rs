//! Azure Cosmos DB adapter for the Quarb query engine.
//!
//! A database's containers at the root, their documents below —
//! Cosmos documents are plain JSON, so the arbor mapping is the
//! JSON adapter's, with the document's `id` as its name. System
//! fields (`_rid`, `_etag`, `_ts`, …) stay out of the tree and
//! surface as adapter metadata instead: `;;;ts` (the epoch
//! last-modified), `;;;rid`, `;;;etag`.
//!
//! **References.** Cosmos declares no typed pointers, so `->`
//! enumerates nothing; resolve with a hint naming the target
//! container follows by-convention ids —
//! `::artist_id~>artists` runs one cross-partition
//! `SELECT * FROM c WHERE c.id = @v`. Hint-less resolution tries
//! the property's `_id` stem, pluralized bare.
//!
//! Everything loads lazily: container names at connect, a
//! container's documents on first descent (paginated, id-sorted
//! for deterministic listings). Read-only, as always.
//!
//! **Target**: `cosmos://ACCOUNT/DATABASE[?endpoint=URL]` — the
//! master (or read-only) key comes from `AZURE_COSMOS_KEY`
//! (base64, as the portal hands it out); `endpoint=URL` points
//! at an emulator or mock.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;

/// An error connecting to or reading Cosmos DB.
#[derive(Debug, thiserror::Error)]
pub enum CosmosError {
    #[error("cosmos: {0}")]
    Api(String),
    #[error("cosmos target: {0} (expected cosmos://ACCOUNT/DATABASE[?endpoint=URL])")]
    Target(String),
    #[error("cosmos: AZURE_COSMOS_KEY is not set (the account's base64 key)")]
    NoKey,
}

enum Kind {
    Root,
    Container { name: String },
    Doc { container: String, body: serde_json::Value },
    Field { value: serde_json::Value },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A Cosmos database, exposed as an arbor.
pub struct CosmosAdapter {
    key: Vec<u8>,
    endpoint: String,
    database: String,
    nodes: RefCell<Vec<Node>>,
}

impl CosmosAdapter {
    /// Connect to `cosmos://ACCOUNT/DATABASE`; one collection
    /// listing probes the account.
    pub fn connect(target: &str) -> Result<Self, CosmosError> {
        let rest = target
            .strip_prefix("cosmos://")
            .ok_or_else(|| CosmosError::Target(target.to_string()))?;
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let (account, database) = path
            .split_once('/')
            .filter(|(a, d)| !a.is_empty() && !d.is_empty())
            .ok_or_else(|| CosmosError::Target(target.to_string()))?;
        let param = |k: &str| {
            query.and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            })
        };
        let endpoint = param("endpoint")
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://{account}.documents.azure.com"));
        let key = std::env::var("AZURE_COSMOS_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .and_then(|k| quarb::base64_decode(k.trim()))
            .ok_or(CosmosError::NoKey)?;
        let adapter = CosmosAdapter {
            key,
            endpoint,
            database: database.to_string(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        let db = adapter.database.clone();
        let resp = adapter.get(
            &format!("dbs/{db}/colls"),
            "colls",
            &format!("dbs/{db}"),
            &[],
        )?;
        let mut names: Vec<String> = resp
            .pointer("/DocumentCollections")
            .and_then(|v| v.as_array())
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| c.pointer("/id").and_then(|v| v.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let ids: Vec<NodeId> = names
            .into_iter()
            .map(|name| {
                adapter.push(Node {
                    kind: Kind::Container { name: name.clone() },
                    name: Some(name),
                    parent: Some(NodeId(0)),
                    children: RefCell::new(None),
                })
            })
            .collect();
        *adapter.nodes.borrow()[0].children.borrow_mut() = Some(ids);
        Ok(adapter)
    }

    /// A human-readable locator: `/container/id…`.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let nodes = self.nodes.borrow();
            let n = &nodes[id.0 as usize];
            if let Some(name) = &n.name {
                parts.push(name.clone());
            }
            cur = n.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// The master-key authorization for one request.
    fn auth(&self, verb: &str, resource_type: &str, resource_link: &str, date: &str) -> String {
        let string_to_sign = format!(
            "{}\n{}\n{}\n{}\n\n",
            verb.to_lowercase(),
            resource_type,
            resource_link,
            date.to_lowercase()
        );
        let sig = quarb::base64(&hmac_sha256(&self.key, string_to_sign.as_bytes()));
        // The header value is the URL-encoded token triple.
        let raw = format!("type=master&ver=1.0&sig={sig}");
        let mut out = String::new();
        for b in raw.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }

    fn get(
        &self,
        path: &str,
        resource_type: &str,
        resource_link: &str,
        extra: &[(&str, &str)],
    ) -> Result<serde_json::Value, CosmosError> {
        self.request("GET", path, resource_type, resource_link, None, extra)
    }

    fn request(
        &self,
        verb: &str,
        path: &str,
        resource_type: &str,
        resource_link: &str,
        body: Option<&str>,
        extra: &[(&str, &str)],
    ) -> Result<serde_json::Value, CosmosError> {
        let date = rfc1123_now();
        let auth = self.auth(verb, resource_type, resource_link, &date);
        let url = format!("{}/{path}", self.endpoint);
        let mut req = match verb {
            "POST" => ureq::post(&url),
            _ => ureq::get(&url),
        };
        req = req
            .set("authorization", &auth)
            .set("x-ms-date", &date)
            .set("x-ms-version", "2018-12-31");
        for (k, v) in extra {
            req = req.set(k, v);
        }
        let result = match body {
            Some(b) => req.send_string(b),
            None => req.call(),
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/message")
                            .and_then(|m| m.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or(text);
                return Err(CosmosError::Api(format!("{verb} {path}: {code}: {msg}")));
            }
            Err(e) => return Err(CosmosError::Api(format!("{verb} {path}: {e}"))),
        };
        let continuation = resp.header("x-ms-continuation").map(str::to_string);
        let mut v: serde_json::Value = serde_json::from_str(
            &resp
                .into_string()
                .map_err(|e| CosmosError::Api(format!("{verb} {path}: reading: {e}")))?,
        )
        .map_err(|e| CosmosError::Api(format!("{verb} {path}: parsing: {e}")))?;
        if let (Some(c), Some(obj)) = (continuation, v.as_object_mut()) {
            obj.insert("__continuation".into(), c.into());
        }
        Ok(v)
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    fn kids_of(&self, node: NodeId) -> Vec<NodeId> {
        enum Plan {
            Done(Vec<NodeId>),
            Leaf,
            Container(String),
            Doc(serde_json::Value),
            Field(serde_json::Value),
        }
        let plan = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            if let Some(k) = &*n.children.borrow() {
                Plan::Done(k.clone())
            } else {
                match &n.kind {
                    Kind::Root => Plan::Leaf,
                    Kind::Container { name } => Plan::Container(name.clone()),
                    Kind::Doc { body, .. } => Plan::Doc(body.clone()),
                    Kind::Field { value } => Plan::Field(value.clone()),
                }
            }
        };
        let made = match plan {
            Plan::Done(k) => return k,
            Plan::Leaf => Vec::new(),
            Plan::Container(name) => self.docs_of(&name, node).unwrap_or_default(),
            Plan::Doc(body) => self.json_children(&body, node, true),
            Plan::Field(value) => self.json_children(&value, node, false),
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(made.clone());
        made
    }

    fn json_children(
        &self,
        v: &serde_json::Value,
        parent: NodeId,
        skip_system: bool,
    ) -> Vec<NodeId> {
        match v {
            serde_json::Value::Object(o) => o
                .iter()
                .filter(|(k, _)| !(skip_system && k.starts_with('_')))
                .map(|(k, val)| {
                    self.push(Node {
                        kind: Kind::Field { value: val.clone() },
                        name: Some(k.clone()),
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            serde_json::Value::Array(a) => a
                .iter()
                .map(|val| {
                    self.push(Node {
                        kind: Kind::Field { value: val.clone() },
                        name: None,
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// One container's documents (paginated, id-sorted).
    fn docs_of(&self, container: &str, parent: NodeId) -> Result<Vec<NodeId>, CosmosError> {
        let db = &self.database;
        let link = format!("dbs/{db}/colls/{container}");
        let mut docs: Vec<serde_json::Value> = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut extra: Vec<(&str, &str)> = Vec::new();
            let c = continuation.clone();
            if let Some(c) = &c {
                extra.push(("x-ms-continuation", c.as_str()));
            }
            let resp = self.get(&format!("{link}/docs"), "docs", &link, &extra)?;
            if let Some(ds) = resp.pointer("/Documents").and_then(|v| v.as_array()) {
                docs.extend(ds.iter().cloned());
            }
            continuation = resp
                .pointer("/__continuation")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if continuation.is_none() {
                break;
            }
        }
        docs.sort_by_key(|d| {
            d.pointer("/id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        });
        Ok(docs
            .into_iter()
            .map(|d| {
                let id = d
                    .pointer("/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.push(Node {
                    kind: Kind::Doc {
                        container: container.to_string(),
                        body: d,
                    },
                    name: Some(id),
                    parent: Some(parent),
                    children: RefCell::new(None),
                })
            })
            .collect())
    }

    /// One document by id, via a cross-partition query.
    fn fetch(&self, container: &str, id_value: &Value) -> Option<NodeId> {
        let db = &self.database;
        let link = format!("dbs/{db}/colls/{container}");
        let body = serde_json::json!({
            "query": "SELECT * FROM c WHERE c.id = @v",
            "parameters": [{ "name": "@v", "value": id_value.to_string() }],
        })
        .to_string();
        let resp = self
            .request(
                "POST",
                &format!("{link}/docs"),
                "docs",
                &link,
                Some(&body),
                &[
                    ("x-ms-documentdb-isquery", "true"),
                    ("content-type", "application/query+json"),
                    ("x-ms-documentdb-query-enablecrosspartition", "true"),
                ],
            )
            .ok()?;
        let doc = resp.pointer("/Documents/0").cloned()?;
        let id = doc
            .pointer("/id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Some(self.push(Node {
            kind: Kind::Doc {
                container: container.to_string(),
                body: doc,
            },
            name: Some(id),
            parent: None,
            children: RefCell::new(None),
        }))
    }
}

fn scalar(v: &serde_json::Value) -> Option<Value> {
    match v {
        serde_json::Value::String(s) => Some(Value::Str(s.clone())),
        serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
        serde_json::Value::Number(n) => Some(if let Some(i) = n.as_i64() {
            Value::Int(i)
        } else {
            Value::Float(n.as_f64().unwrap_or(f64::NAN))
        }),
        serde_json::Value::Null => Some(Value::Null),
        _ => None,
    }
}

impl AstAdapter for CosmosAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.kids_of(node)
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
            Kind::Container { .. } => vec!["container".into()],
            Kind::Doc { .. } => vec!["document".into()],
            Kind::Field { .. } => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { body, .. } => {
                if name.starts_with('_') {
                    return None;
                }
                body.pointer(&format!("/{name}")).and_then(scalar)
            }
            Kind::Field { value } => value.pointer(&format!("/{name}")).and_then(scalar),
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field { value } => scalar(value),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { body, .. } => match key {
                "ts" => body.pointer("/_ts").and_then(scalar),
                "rid" => body.pointer("/_rid").and_then(scalar),
                "etag" => body.pointer("/_etag").and_then(scalar),
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let value = self.property(node, property)?;
        let candidates: Vec<String> = match hint {
            Some(h) => vec![h.to_string()],
            None => {
                let stem = property.strip_suffix("_id").unwrap_or(property);
                vec![format!("{stem}s"), stem.to_string()]
            }
        };
        let containers: Vec<String> = {
            let nodes = self.nodes.borrow();
            let root_kids = nodes[0].children.borrow().clone().unwrap_or_default();
            root_kids
                .iter()
                .filter_map(|id| nodes[id.0 as usize].name.clone())
                .collect()
        };
        for c in candidates {
            if containers.iter().any(|t| t == &c)
                && let Some(id) = self.fetch(&c, &value)
            {
                return Some(id);
            }
        }
        None
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&quarb::sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = quarb::sha256(&[ipad.as_slice(), msg].concat());
    quarb::sha256(&[opad.as_slice(), &inner].concat())
}

/// The current instant as an RFC 1123 date, for `x-ms-date`.
fn rfc1123_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs();
    let days = (secs / 86400) as i64;
    let (h, mi, sec) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        [((days + 4).rem_euclid(7)) as usize];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(mo - 1) as usize];
    format!("{weekday}, {d:02} {month} {y} {h:02}:{mi:02}:{sec:02} GMT")
}
