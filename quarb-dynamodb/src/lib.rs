//! Amazon DynamoDB adapter for the Quarb query engine.
//!
//! An account's tables at the root, a table's items as its
//! children, and each item's attribute tree below it — DynamoDB
//! attribute values are already tree-shaped (maps and lists all
//! the way down), so the arbor mapping is the identity the
//! format was waiting for.
//!
//! **Naming.** Items are named by their partition (hash) key
//! value. Tables with a sort (range) key may repeat a name —
//! exactly as `/row` repeats in CSV — and the sort key stays a
//! property to filter on:
//! `/music/'Beatles'[::song = "Help!"]::year`. `;;;hash-key` /
//! `;;;range-key` on a table name the schema; `:::index` is the
//! stable position within the (key-sorted, deterministic) scan.
//!
//! **References.** DynamoDB declares no foreign keys, so `->`
//! enumerates nothing; the by-convention pointers single-table
//! and multi-table designs use resolve with a hint naming the
//! target table: `::artist_id~>artists` fetches the item whose
//! partition key equals the value (a `GetItem`, not a scan).
//! Hint-less resolution falls back to the property name minus a
//! trailing `_id`, pluralized bare (`::artist_id~>` tries the
//! `artist` and `artists` tables).
//!
//! **Scalars are properties AND leaves** (the dual-exposure
//! doctrine): `::year` reads a top-level attribute; `/year` is
//! the same value as a leaf; maps and lists descend as
//! subtrees.
//!
//! Everything loads lazily: table names at connect, a table's
//! key schema on first touch, its items on first descent (one
//! paginated `Scan`, then sorted by key so listings are
//! deterministic). Read-only, as always.
//!
//! **Target**: `dynamodb://[REGION][?endpoint=URL]` — region
//! from the target, else the standard chain (`AWS_REGION`,
//! `~/.aws/config`, `us-east-1`); credentials from the standard
//! chain (see `quarb-aws`). `endpoint=URL` points at
//! DynamoDB Local or any compatible endpoint.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading DynamoDB.
#[derive(Debug, thiserror::Error)]
pub enum DynamodbError {
    #[error("dynamodb: {0}")]
    Api(String),
    #[error("dynamodb target: {0} (expected dynamodb://[REGION][?endpoint=URL])")]
    Target(String),
    #[error("dynamodb: no credentials in the chain (set AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or ~/.aws/credentials)")]
    NoCredentials,
}

/// One attribute value, decoded from the wire's tagged form.
#[derive(Clone)]
enum Field {
    Scalar(Value),
    Map(Vec<(String, Field)>),
    List(Vec<Field>),
}

enum Kind {
    Root,
    Table {
        name: String,
    },
    /// An item: its table, its display name (hash-key value),
    /// and its decoded attributes.
    Item {
        table: String,
        attrs: Vec<(String, Field)>,
    },
    /// A node inside an item's attribute tree.
    Field {
        value: Field,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A DynamoDB endpoint, exposed as an arbor.
pub struct DynamodbAdapter {
    creds: quarb_aws::Credentials,
    region: String,
    endpoint: String,
    nodes: RefCell<Vec<Node>>,
    /// table → (hash key, range key) once described.
    schema: RefCell<HashMap<String, (String, Option<String>)>>,
}

impl DynamodbAdapter {
    /// Connect to `dynamodb://…`; one `ListTables` probes the
    /// endpoint and fills the root.
    pub fn connect(target: &str) -> Result<Self, DynamodbError> {
        let rest = target
            .strip_prefix("dynamodb://")
            .or_else(|| target.strip_prefix("dynamodb:"))
            .ok_or_else(|| DynamodbError::Target(target.to_string()))?;
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let param = |k: &str| {
            query.and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            })
        };
        let region = quarb_aws::region(if path.is_empty() { None } else { Some(path) });
        let endpoint = param("endpoint")
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://dynamodb.{region}.amazonaws.com"));
        let creds = quarb_aws::load_credentials().ok_or(DynamodbError::NoCredentials)?;
        let adapter = DynamodbAdapter {
            creds,
            region,
            endpoint,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            schema: RefCell::new(HashMap::new()),
        };
        // Probe: list the tables now, so a bad endpoint fails at
        // connect rather than mid-query.
        let mut names = Vec::new();
        let mut start: Option<String> = None;
        loop {
            let mut body = serde_json::Map::new();
            if let Some(s) = &start {
                body.insert("ExclusiveStartTableName".into(), s.clone().into());
            }
            let resp = adapter.call("ListTables", &serde_json::Value::Object(body))?;
            if let Some(ts) = resp.pointer("/TableNames").and_then(|v| v.as_array()) {
                names.extend(ts.iter().filter_map(|t| t.as_str().map(str::to_string)));
            }
            start = resp
                .pointer("/LastEvaluatedTableName")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if start.is_none() {
                break;
            }
        }
        names.sort();
        let ids: Vec<NodeId> = names
            .into_iter()
            .map(|name| {
                adapter.push(Node {
                    kind: Kind::Table { name: name.clone() },
                    name: Some(name),
                    parent: Some(NodeId(0)),
                    children: RefCell::new(None),
                })
            })
            .collect();
        *adapter.nodes.borrow()[0].children.borrow_mut() = Some(ids);
        Ok(adapter)
    }

    /// A human-readable locator: `table/hash-key-value…`.
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

    fn call(
        &self,
        op: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, DynamodbError> {
        let payload = body.to_string();
        let target = format!("DynamoDB_20120810.{op}");
        let extra = [
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", target.as_str()),
        ];
        let headers = quarb_aws::sign(
            &self.creds,
            "POST",
            &self.endpoint,
            &self.region,
            "dynamodb",
            payload.as_bytes(),
            &extra,
        );
        let mut req = ureq::post(&self.endpoint);
        for (k, v) in &headers {
            if k != "host" {
                req = req.set(k, v);
            }
        }
        let resp = match req.send_string(&payload) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/message")
                            .or_else(|| v.pointer("/Message"))
                            .and_then(|m| m.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or(text);
                return Err(DynamodbError::Api(format!("{op}: {code}: {msg}")));
            }
            Err(e) => return Err(DynamodbError::Api(format!("{op}: {e}"))),
        };
        serde_json::from_str(&resp.into_string().map_err(|e| {
            DynamodbError::Api(format!("{op}: reading response: {e}"))
        })?)
        .map_err(|e| DynamodbError::Api(format!("{op}: parsing response: {e}")))
    }

    /// The table's (hash, range) key names, described once.
    fn keys_of(&self, table: &str) -> Result<(String, Option<String>), DynamodbError> {
        if let Some(k) = self.schema.borrow().get(table) {
            return Ok(k.clone());
        }
        let resp = self.call(
            "DescribeTable",
            &serde_json::json!({ "TableName": table }),
        )?;
        let mut hash = String::new();
        let mut range = None;
        if let Some(ks) = resp
            .pointer("/Table/KeySchema")
            .and_then(|v| v.as_array())
        {
            for k in ks {
                let name = k
                    .pointer("/AttributeName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                match k.pointer("/KeyType").and_then(|v| v.as_str()) {
                    Some("HASH") => hash = name,
                    Some("RANGE") => range = Some(name),
                    _ => {}
                }
            }
        }
        let out = (hash, range);
        self.schema
            .borrow_mut()
            .insert(table.to_string(), out.clone());
        Ok(out)
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// Materialize a container's children, lazily. The plan is
    /// extracted first so the arena borrow is released before
    /// any child pushes re-borrow it mutably.
    fn kids_of(&self, node: NodeId) -> Vec<NodeId> {
        enum Plan {
            Done(Vec<NodeId>),
            Leaf,
            Table(String),
            Entries(Vec<(String, Field)>),
            List(Vec<Field>),
        }
        let plan = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            if let Some(k) = &*n.children.borrow() {
                Plan::Done(k.clone())
            } else {
                match &n.kind {
                    Kind::Root => Plan::Leaf, // filled at connect
                    Kind::Table { name } => Plan::Table(name.clone()),
                    Kind::Item { attrs, .. } => Plan::Entries(attrs.clone()),
                    Kind::Field { value } => match value {
                        Field::Map(entries) => Plan::Entries(entries.clone()),
                        Field::List(items) => Plan::List(items.clone()),
                        Field::Scalar(_) => Plan::Leaf,
                    },
                }
            }
        };
        let made = match plan {
            Plan::Done(k) => return k,
            Plan::Leaf => Vec::new(),
            Plan::Table(name) => self.scan(name, node).unwrap_or_default(),
            Plan::Entries(entries) => self.field_children(&entries, node),
            Plan::List(items) => items
                .into_iter()
                .map(|f| {
                    self.push(Node {
                        kind: Kind::Field { value: f },
                        name: None,
                        parent: Some(node),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(made.clone());
        made
    }

    fn field_children(&self, entries: &[(String, Field)], parent: NodeId) -> Vec<NodeId> {
        entries
            .iter()
            .map(|(k, f)| {
                self.push(Node {
                    kind: Kind::Field { value: f.clone() },
                    name: Some(k.clone()),
                    parent: Some(parent),
                    children: RefCell::new(None),
                })
            })
            .collect()
    }

    /// One full (paginated) scan of a table, key-sorted for
    /// deterministic listings.
    fn scan(&self, table: String, parent: NodeId) -> Result<Vec<NodeId>, DynamodbError> {
        let (hash, range) = self.keys_of(&table)?;
        let mut raw: Vec<serde_json::Value> = Vec::new();
        let mut start: Option<serde_json::Value> = None;
        loop {
            let mut body = serde_json::Map::new();
            body.insert("TableName".into(), table.clone().into());
            if let Some(s) = &start {
                body.insert("ExclusiveStartKey".into(), s.clone());
            }
            let resp = self.call("Scan", &serde_json::Value::Object(body))?;
            if let Some(items) = resp.pointer("/Items").and_then(|v| v.as_array()) {
                raw.extend(items.iter().cloned());
            }
            start = resp.pointer("/LastEvaluatedKey").cloned().filter(|v| {
                v.as_object().map(|o| !o.is_empty()).unwrap_or(false)
            });
            if start.is_none() {
                break;
            }
        }
        let key_of = |item: &serde_json::Value| {
            let h = item
                .pointer(&format!("/{hash}"))
                .map(decode_scalar_text)
                .unwrap_or_default();
            let r = range
                .as_ref()
                .and_then(|r| item.pointer(&format!("/{r}")))
                .map(decode_scalar_text)
                .unwrap_or_default();
            (h, r)
        };
        raw.sort_by_key(key_of);
        Ok(raw
            .into_iter()
            .map(|item| {
                let (h, _) = key_of(&item);
                let attrs = decode_map(&item);
                self.push(Node {
                    kind: Kind::Item {
                        table: table.clone(),
                        attrs,
                    },
                    name: Some(h),
                    parent: Some(parent),
                    children: RefCell::new(None),
                })
            })
            .collect())
    }

    /// Fetch one item by partition key (`GetItem` needs the full
    /// primary key, so tables with a range key resolve via a
    /// key-condition `Query` instead, taking the first item).
    fn fetch(&self, table: &str, key_value: &Value) -> Option<NodeId> {
        let (hash, range) = self.keys_of(table).ok()?;
        if hash.is_empty() {
            return None;
        }
        let attr = encode_scalar(key_value);
        let item = if range.is_none() {
            self.call(
                "GetItem",
                &serde_json::json!({ "TableName": table, "Key": { hash.clone(): attr } }),
            )
            .ok()?
            .pointer("/Item")
            .cloned()?
        } else {
            let resp = self
                .call(
                    "Query",
                    &serde_json::json!({
                        "TableName": table,
                        "KeyConditionExpression": "#k = :v",
                        "ExpressionAttributeNames": { "#k": hash },
                        "ExpressionAttributeValues": { ":v": attr },
                        "Limit": 1,
                    }),
                )
                .ok()?;
            resp.pointer("/Items/0").cloned()?
        };
        if !item.is_object() || item.as_object().is_some_and(|o| o.is_empty()) {
            return None;
        }
        let attrs = decode_map(&item);
        let name = item
            .pointer(&format!("/{hash}"))
            .map(decode_scalar_text)
            .unwrap_or_default();
        Some(self.push(Node {
            kind: Kind::Item {
                table: table.to_string(),
                attrs,
            },
            name: Some(name),
            parent: None,
            children: RefCell::new(None),
        }))
    }
}

/// Decode a wire attribute value (`{"S": …}`, `{"N": …}`, …).
fn decode(av: &serde_json::Value) -> Field {
    let Some(obj) = av.as_object() else {
        return Field::Scalar(Value::Null);
    };
    let Some((tag, inner)) = obj.iter().next() else {
        return Field::Scalar(Value::Null);
    };
    match tag.as_str() {
        "S" => Field::Scalar(Value::Str(inner.as_str().unwrap_or_default().into())),
        "N" => Field::Scalar(number(inner.as_str().unwrap_or_default())),
        "BOOL" => Field::Scalar(Value::Bool(inner.as_bool().unwrap_or(false))),
        "NULL" => Field::Scalar(Value::Null),
        "B" => Field::Scalar(Value::Str(inner.as_str().unwrap_or_default().into())),
        "M" => Field::Map(decode_map(inner)),
        "L" => Field::List(
            inner
                .as_array()
                .map(|a| a.iter().map(decode).collect())
                .unwrap_or_default(),
        ),
        "SS" | "BS" => Field::List(
            inner
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| {
                            Field::Scalar(Value::Str(
                                s.as_str().unwrap_or_default().into(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "NS" => Field::List(
            inner
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|s| Field::Scalar(number(s.as_str().unwrap_or_default())))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => Field::Scalar(Value::Null),
    }
}

fn decode_map(m: &serde_json::Value) -> Vec<(String, Field)> {
    let mut out: Vec<(String, Field)> = m
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        .unwrap_or_default();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The display text of a wire scalar (for item names and sort
/// keys).
fn decode_scalar_text(av: &serde_json::Value) -> String {
    match decode(av) {
        Field::Scalar(v) => v.to_string(),
        _ => String::new(),
    }
}

/// Encode a Quarb value as the wire scalar for key lookups.
fn encode_scalar(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(n) => serde_json::json!({ "N": n.to_string() }),
        Value::Float(f) => serde_json::json!({ "N": f.to_string() }),
        Value::Bool(b) => serde_json::json!({ "BOOL": b }),
        other => serde_json::json!({ "S": other.to_string() }),
    }
}

fn number(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        Value::Int(i)
    } else if let Ok(f) = s.parse::<f64>() {
        Value::Float(f)
    } else {
        Value::Str(s.to_string())
    }
}

impl Field {
    fn scalar(&self) -> Option<Value> {
        match self {
            Field::Scalar(v) => Some(v.clone()),
            _ => None,
        }
    }
}

impl AstAdapter for DynamodbAdapter {
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
            Kind::Table { .. } => vec!["table".into()],
            Kind::Item { .. } => vec!["item".into()],
            Kind::Field { .. } => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Item { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar()),
            Kind::Field { value: Field::Map(entries) } => entries
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar()),
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field { value } => value.scalar(),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let (table_name, item_table) = {
            let nodes = self.nodes.borrow();
            match &nodes[node.0 as usize].kind {
                Kind::Table { name } => (Some(name.clone()), None),
                Kind::Item { table, .. } => (None, Some(table.clone())),
                _ => (None, None),
            }
        };
        match (table_name, item_table, key) {
            (Some(t), _, "hash-key") => {
                self.keys_of(&t).ok().map(|(h, _)| Value::Str(h))
            }
            (Some(t), _, "range-key") => {
                self.keys_of(&t).ok().and_then(|(_, r)| r.map(Value::Str))
            }
            (_, Some(t), "table") => Some(Value::Str(t)),
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
        let tables: Vec<String> = {
            let nodes = self.nodes.borrow();
            let root_kids = nodes[0].children.borrow().clone().unwrap_or_default();
            root_kids
                .iter()
                .filter_map(|id| nodes[id.0 as usize].name.clone())
                .collect()
        };
        for c in candidates {
            if tables.iter().any(|t| t == &c) {
                if let Some(id) = self.fetch(&c, &value) {
                    return Some(id);
                }
            }
        }
        None
    }
}
