//! Redis adapter for the Quarb query engine.
//!
//! Redis looks flat — one keyspace, opaque values — but practice
//! gave it both a tree and types. The universal `a:b:c` **key
//! namespacing convention** is the tree: the adapter splits keys
//! on `:`, so `user:42:visits` is the path `/user/42/visits`, and
//! a segment that is both a key and a prefix (`user:42` a hash,
//! `user:42:visits` a list) is one node with both facets. The
//! **value types** are the shapes below the key:
//!
//! - *string* — a leaf; JSON payloads graft open as subtrees
//!   (the dual-exposure doctrine applies);
//! - *hash* — fields as named children **and** as `::field`
//!   properties of the key node;
//! - *list* — ordered unnamed children;
//! - *set* — members, sorted (deterministic listings);
//! - *zset* — members in score order, the score at `;;;score`;
//! - *stream* — a **bounded snapshot** (the Kafka ruling): the
//!   entries present at first touch, named by entry id, their
//!   id's clock minted as a typed `;;;ts` instant, fields as
//!   children and properties.
//!
//! **References.** Values that hold key names are Redis's foreign
//! keys by convention. Resolve follows them: hint-less `~>` when
//! the value *is* a full key (`::next~>` on `"user:42"`), hinted
//! `~>user` when the value is the tail (`42` under the `user:`
//! prefix).
//!
//! `;;;type` names a key's Redis type, `;;;ttl` is a typed
//! duration (`[;;;ttl < 1h]` finds what expires soon), `;;;key`
//! is the full key. The keyspace is scanned once at connect and
//! sorted; values load lazily per key. Read-only, as always.
//!
//! **Target**: `redis://[USER:PASS@]HOST[:PORT][/DB]` (or
//! `rediss://` for TLS) — the standard Redis URL. `?scan=GLOB`
//! narrows the keyspace scan.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;

/// An error connecting to or reading Redis.
#[derive(Debug, thiserror::Error)]
pub enum RedisError {
    #[error("redis: {0}")]
    Api(String),
    #[error("redis target: {0} (expected redis://[USER:PASS@]HOST[:PORT][/DB])")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> RedisError {
    RedisError::Api(e.to_string())
}

/// One decoded value node: a JSON tree or a scalar leaf (the
/// kafka adapter's payload pattern).
#[derive(Clone)]
enum Field {
    Scalar(Value),
    Map(Vec<(String, Field)>),
    List(Vec<Field>),
}

impl Field {
    fn scalar(&self) -> Option<Value> {
        match self {
            Field::Scalar(v) => Some(v.clone()),
            _ => None,
        }
    }
}

/// Text (a string value, hash field, list item, …) as a Field:
/// JSON grafts, everything else stays a text leaf.
fn decode_text(text: &str) -> Field {
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(text)
    {
        return decode_json(&v);
    }
    Field::Scalar(Value::Str(text.to_string()))
}

fn decode_json(v: &serde_json::Value) -> Field {
    match v {
        serde_json::Value::Null => Field::Scalar(Value::Null),
        serde_json::Value::Bool(b) => Field::Scalar(Value::Bool(*b)),
        serde_json::Value::Number(n) => Field::Scalar(if let Some(i) = n.as_i64() {
            Value::Int(i)
        } else {
            Value::Float(n.as_f64().unwrap_or(f64::NAN))
        }),
        serde_json::Value::String(s) => Field::Scalar(Value::Str(s.clone())),
        serde_json::Value::Array(a) => Field::List(a.iter().map(decode_json).collect()),
        serde_json::Value::Object(o) => {
            Field::Map(o.iter().map(|(k, v)| (k.clone(), decode_json(v))).collect())
        }
    }
}

/// What a key's loaded content looks like.
enum Content {
    /// string — the payload (JSON grafted or a text leaf).
    Str(Field),
    /// hash — (field, payload), sorted by field.
    Hash(Vec<(String, Field)>),
    /// list — payloads in list order.
    List(Vec<Field>),
    /// set — members, sorted.
    Set(Vec<Field>),
    /// zset — (member, score) in score order.
    ZSet(Vec<(String, f64)>),
    /// stream — (id, ms, fields) in id order.
    Stream(Vec<(String, i64, Vec<(String, Field)>)>),
    Other,
}

enum Kind {
    Root,
    /// A path segment: possibly a full key, possibly (also) a
    /// namespace with segment children.
    Path {
        key: Option<String>,
        /// Set once the key's content children materialized.
        loaded: RefCell<bool>,
    },
    /// A hash field / stream field / JSON object entry.
    Named { value: Field },
    /// A list item / set member / JSON array element.
    Item { value: Field },
    /// A zset member.
    Scored { member: String, score: f64 },
    /// A stream entry: its clock and fields.
    Entry {
        ms: i64,
        fields: Vec<(String, Field)>,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A Redis keyspace, exposed as an arbor.
pub struct RedisAdapter {
    conn: RefCell<redis::Connection>,
    nodes: RefCell<Vec<Node>>,
    /// full key → its Path node.
    by_key: RefCell<HashMap<String, NodeId>>,
    /// key → TYPE, cached.
    types: RefCell<HashMap<String, String>>,
}

impl RedisAdapter {
    /// Connect to `redis://…`; one full keyspace SCAN builds the
    /// namespace tree (sorted — SCAN order is not deterministic).
    pub fn connect(target: &str) -> Result<Self, RedisError> {
        if !target.starts_with("redis://") && !target.starts_with("rediss://") {
            return Err(RedisError::Target(target.to_string()));
        }
        let (url, scan) = match target.split_once('?') {
            Some((u, q)) => (
                u.to_string(),
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("scan=").map(str::to_string)),
            ),
            None => (target.to_string(), None),
        };
        let client = redis::Client::open(url.as_str()).map_err(api)?;
        let mut conn = client.get_connection().map_err(api)?;
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let mut cmd = redis::cmd("SCAN");
            cmd.arg(cursor).arg("COUNT").arg(1000);
            if let Some(m) = &scan {
                cmd.arg("MATCH").arg(m);
            }
            let (next, batch): (u64, Vec<String>) = cmd.query(&mut conn).map_err(api)?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        keys.sort();
        keys.dedup();
        let adapter = RedisAdapter {
            conn: RefCell::new(conn),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_key: RefCell::new(HashMap::new()),
            types: RefCell::new(HashMap::new()),
        };
        // Build the segment tree eagerly (content stays lazy).
        let mut by_path: BTreeMap<String, NodeId> = BTreeMap::new();
        for key in &keys {
            let mut path = String::new();
            let mut parent = NodeId(0);
            let segs: Vec<&str> = key.split(':').collect();
            for (i, seg) in segs.iter().enumerate() {
                if !path.is_empty() {
                    path.push(':');
                }
                path.push_str(seg);
                let id = match by_path.get(&path) {
                    Some(&id) => id,
                    None => {
                        let mut nodes = adapter.nodes.borrow_mut();
                        let id = NodeId(nodes.len() as u64);
                        nodes.push(Node {
                            kind: Kind::Path {
                                key: None,
                                loaded: RefCell::new(false),
                            },
                            name: Some(seg.to_string()),
                            parent: Some(parent),
                            children: RefCell::new(None),
                        });
                        drop(nodes);
                        by_path.insert(path.clone(), id);
                        id
                    }
                };
                if i == segs.len() - 1 {
                    if let Kind::Path { key: k, .. } =
                        &mut adapter.nodes.borrow_mut()[id.0 as usize].kind
                    {
                        *k = Some(key.clone());
                    }
                    adapter.by_key.borrow_mut().insert(key.clone(), id);
                }
                parent = id;
            }
        }
        // Wire up segment children (sorted by name).
        let mut kids: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for (_, &id) in by_path.iter() {
            let parent = adapter.nodes.borrow()[id.0 as usize].parent.unwrap();
            kids.entry(parent).or_default().push(id);
        }
        for (parent, mut list) in kids {
            {
                let nodes = adapter.nodes.borrow();
                list.sort_by(|a, b| nodes[a.0 as usize].name.cmp(&nodes[b.0 as usize].name));
            }
            *adapter.nodes.borrow()[parent.0 as usize].children.borrow_mut() = Some(list);
        }
        Ok(adapter)
    }

    /// A human-readable locator: the colon-joined key path.
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

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// TYPE, once per key.
    fn type_of(&self, key: &str) -> String {
        if let Some(t) = self.types.borrow().get(key) {
            return t.clone();
        }
        let t: String = redis::cmd("TYPE")
            .arg(key)
            .query(&mut self.conn.borrow_mut())
            .unwrap_or_else(|_| "none".into());
        self.types.borrow_mut().insert(key.to_string(), t.clone());
        t
    }

    /// Load a key's content by type.
    fn content_of(&self, key: &str) -> Content {
        let ty = self.type_of(key);
        let mut conn = self.conn.borrow_mut();
        match ty.as_str() {
            "string" => {
                let s: Option<Vec<u8>> = redis::cmd("GET").arg(key).query(&mut conn).ok();
                match s {
                    Some(bytes) => match std::str::from_utf8(&bytes) {
                        Ok(text) => Content::Str(decode_text(text)),
                        Err(_) => Content::Str(Field::Scalar(Value::Null)),
                    },
                    None => Content::Other,
                }
            }
            "hash" => {
                let mut pairs: Vec<(String, String)> = redis::cmd("HGETALL")
                    .arg(key)
                    .query(&mut conn)
                    .unwrap_or_default();
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                Content::Hash(
                    pairs
                        .into_iter()
                        .map(|(k, v)| (k, decode_text(&v)))
                        .collect(),
                )
            }
            "list" => {
                let items: Vec<String> = redis::cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(-1)
                    .query(&mut conn)
                    .unwrap_or_default();
                Content::List(items.iter().map(|s| decode_text(s)).collect())
            }
            "set" => {
                let mut items: Vec<String> = redis::cmd("SMEMBERS")
                    .arg(key)
                    .query(&mut conn)
                    .unwrap_or_default();
                items.sort();
                Content::Set(items.iter().map(|s| decode_text(s)).collect())
            }
            "zset" => {
                let pairs: Vec<(String, f64)> = redis::cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(-1)
                    .arg("WITHSCORES")
                    .query(&mut conn)
                    .unwrap_or_default();
                Content::ZSet(pairs)
            }
            "stream" => {
                let reply: redis::Value = redis::cmd("XRANGE")
                    .arg(key)
                    .arg("-")
                    .arg("+")
                    .query(&mut conn)
                    .unwrap_or(redis::Value::Nil);
                Content::Stream(parse_stream(&reply))
            }
            _ => Content::Other,
        }
    }

    /// Children of a Field subtree (the JSON graft).
    fn field_children(&self, f: &Field, parent: NodeId) -> Vec<NodeId> {
        match f {
            Field::Map(entries) => entries
                .iter()
                .map(|(k, v)| {
                    self.push(Node {
                        kind: Kind::Named { value: v.clone() },
                        name: Some(k.clone()),
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Field::List(items) => items
                .iter()
                .map(|v| {
                    self.push(Node {
                        kind: Kind::Item { value: v.clone() },
                        name: None,
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Field::Scalar(_) => Vec::new(),
        }
    }

    /// Materialize a key's content children (appended after any
    /// namespace children the segment tree already wired).
    fn content_children(&self, key: &str, parent: NodeId) -> Vec<NodeId> {
        match self.content_of(key) {
            Content::Str(f) => self.field_children(&f, parent),
            Content::Hash(fields) => fields
                .into_iter()
                .map(|(k, v)| {
                    self.push(Node {
                        kind: Kind::Named { value: v },
                        name: Some(k),
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Content::List(items) | Content::Set(items) => items
                .into_iter()
                .map(|v| {
                    self.push(Node {
                        kind: Kind::Item { value: v },
                        name: None,
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Content::ZSet(pairs) => pairs
                .into_iter()
                .map(|(m, s)| {
                    self.push(Node {
                        name: Some(m.clone()),
                        kind: Kind::Scored { member: m, score: s },
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Content::Stream(entries) => entries
                .into_iter()
                .map(|(id, ms, fields)| {
                    self.push(Node {
                        name: Some(id),
                        kind: Kind::Entry { ms, fields },
                        parent: Some(parent),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Content::Other => Vec::new(),
        }
    }
}

/// XRANGE reply: [[id, [k, v, k, v…]], …].
fn parse_stream(v: &redis::Value) -> Vec<(String, i64, Vec<(String, Field)>)> {
    let text = |v: &redis::Value| -> Option<String> {
        match v {
            redis::Value::BulkString(b) => Some(String::from_utf8_lossy(b).into_owned()),
            redis::Value::SimpleString(s) => Some(s.clone()),
            _ => None,
        }
    };
    let redis::Value::Array(entries) = v else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let redis::Value::Array(pair) = e else { return None };
            let id = text(pair.first()?)?;
            let ms: i64 = id.split('-').next()?.parse().ok()?;
            let redis::Value::Array(kv) = pair.get(1)? else {
                return None;
            };
            let mut fields = Vec::new();
            for chunk in kv.chunks(2) {
                if let (Some(k), Some(vv)) = (text(&chunk[0]), chunk.get(1).and_then(text)) {
                    fields.push((k, decode_text(&vv)));
                }
            }
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Some((id, ms, fields))
        })
        .collect()
}

impl AstAdapter for RedisAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        // Field / entry nodes: materialize their subtree once.
        enum Plan {
            Done(Vec<NodeId>),
            Field(Field),
            Entry(Vec<(String, Field)>),
            Path(Option<String>, bool, Vec<NodeId>),
            Leaf,
        }
        let plan = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            match &n.kind {
                Kind::Path { key, loaded } => Plan::Path(
                    key.clone(),
                    *loaded.borrow(),
                    n.children.borrow().clone().unwrap_or_default(),
                ),
                Kind::Named { value } | Kind::Item { value } => match n.children.borrow().clone()
                {
                    Some(c) => Plan::Done(c),
                    None => Plan::Field(value.clone()),
                },
                Kind::Entry { fields, .. } => match n.children.borrow().clone() {
                    Some(c) => Plan::Done(c),
                    None => Plan::Entry(fields.clone()),
                },
                Kind::Root => Plan::Path(None, true, n.children.borrow().clone().unwrap_or_default()),
                _ => Plan::Leaf,
            }
        };
        match plan {
            Plan::Done(c) => c,
            Plan::Leaf => Vec::new(),
            Plan::Field(f) => {
                let made = self.field_children(&f, node);
                *self.nodes.borrow()[node.0 as usize].children.borrow_mut() =
                    Some(made.clone());
                made
            }
            Plan::Entry(fields) => {
                let made: Vec<NodeId> = fields
                    .into_iter()
                    .map(|(k, v)| {
                        self.push(Node {
                            kind: Kind::Named { value: v },
                            name: Some(k),
                            parent: Some(node),
                            children: RefCell::new(None),
                        })
                    })
                    .collect();
                *self.nodes.borrow()[node.0 as usize].children.borrow_mut() =
                    Some(made.clone());
                made
            }
            Plan::Path(key, loaded, mut existing) => {
                if let Some(key) = key
                    && !loaded
                {
                    let content = self.content_children(&key, node);
                    existing.extend(content);
                    let nodes = self.nodes.borrow();
                    *nodes[node.0 as usize].children.borrow_mut() = Some(existing.clone());
                    if let Kind::Path { loaded, .. } = &nodes[node.0 as usize].kind {
                        *loaded.borrow_mut() = true;
                    }
                }
                existing
            }
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// A key node's trait is its Redis type.
    fn traits(&self, node: NodeId) -> Vec<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Path { key: Some(k), .. } => vec![self.type_of(k)],
            Kind::Entry { .. } => vec!["entry".to_string()],
            _ => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        // Key nodes answer with hash fields / grafted JSON keys —
        // force the content in first.
        let is_key = matches!(
            &self.nodes.borrow()[node.0 as usize].kind,
            Kind::Path { key: Some(_), .. }
        );
        if is_key {
            self.children(node);
        }
        let nodes = self.nodes.borrow();
        match &nodes[node.0 as usize].kind {
            Kind::Path { .. } => {
                let kids = nodes[node.0 as usize].children.borrow().clone()?;
                kids.iter().find_map(|&c| {
                    let n = &nodes[c.0 as usize];
                    if n.name.as_deref() == Some(name) {
                        match &n.kind {
                            Kind::Named { value } => value.scalar(),
                            _ => None,
                        }
                    } else {
                        None
                    }
                })
            }
            Kind::Named { value } | Kind::Item { value } => match value {
                Field::Map(entries) => entries
                    .iter()
                    .find(|(k, _)| k == name)
                    .and_then(|(_, f)| f.scalar()),
                _ => None,
            },
            Kind::Entry { fields, .. } => fields
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar()),
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let key = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Path { key: Some(k), .. } => Some(k.clone()),
            _ => None,
        };
        if let Some(key) = key {
            return match self.content_of(&key) {
                Content::Str(f) => f.scalar(),
                _ => None,
            };
        }
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Named { value } | Kind::Item { value } => value.scalar(),
            Kind::Scored { member, .. } => Some(Value::Str(member.clone())),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let nodes = self.nodes.borrow();
        match (&nodes[node.0 as usize].kind, key) {
            (Kind::Path { key: Some(k), .. }, "type") => {
                let k = k.clone();
                drop(nodes);
                Some(Value::Str(self.type_of(&k)))
            }
            (Kind::Path { key: Some(k), .. }, "key") => Some(Value::Str(k.clone())),
            (Kind::Path { key: Some(k), .. }, "ttl") => {
                let k = k.clone();
                drop(nodes);
                let ttl: i64 = redis::cmd("TTL")
                    .arg(&k)
                    .query(&mut self.conn.borrow_mut())
                    .ok()?;
                (ttl >= 0).then_some(Value::Duration { secs: ttl, nanos: 0 })
            }
            (Kind::Scored { score, .. }, "score") => Some(Value::Float(*score)),
            (Kind::Entry { ms, .. }, "ts") => Some(Value::Instant {
                secs: ms.div_euclid(1000),
                nanos: (ms.rem_euclid(1000) as u32) * 1_000_000,
                offset_min: None,
            }),
            _ => None,
        }
    }

    /// Key-convention references: the value (hint-less) is a full
    /// key; hinted, `~>user` prefixes `user:` onto the value.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let value = self
            .property(node, property)
            .or_else(|| self.default_value(node))?
            .to_string();
        let candidates = match hint {
            Some(h) => vec![format!("{h}:{value}"), value.clone()],
            None => vec![value.clone()],
        };
        let by_key = self.by_key.borrow();
        candidates.iter().find_map(|k| by_key.get(k).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_decoding() {
        assert!(matches!(
            decode_text(r#"{"a": 1}"#),
            Field::Map(ref e) if e.len() == 1
        ));
        assert!(matches!(
            decode_text("plain"),
            Field::Scalar(Value::Str(ref s)) if s == "plain"
        ));
    }

    #[test]
    fn stream_parsing() {
        use redis::Value as R;
        let reply = R::Array(vec![R::Array(vec![
            R::BulkString(b"1753380000123-0".to_vec()),
            R::Array(vec![
                R::BulkString(b"kind".to_vec()),
                R::BulkString(b"temp".to_vec()),
                R::BulkString(b"celsius".to_vec()),
                R::BulkString(b"21.5".to_vec()),
            ]),
        ])]);
        let entries = parse_stream(&reply);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "1753380000123-0");
        assert_eq!(entries[0].1, 1753380000123);
        assert_eq!(entries[0].2.len(), 2);
    }

    #[test]
    fn target_scheme() {
        assert!(RedisAdapter::connect("http://x").is_err());
    }
}
