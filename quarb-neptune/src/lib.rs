//! Amazon Neptune adapter for the Quarb query engine.
//!
//! Neptune's **openCypher HTTPS endpoint** carries the same
//! property-graph model the Neo4j adapter maps, so the mapping is
//! identical: the root holds one child per **label**, a label
//! holds its nodes, and **relationship types become labeled
//! crosslinks** (`->KNOWS`, `<-KNOWS`, `->*`), with relationship
//! properties answering the `$-` edge accessor. `?key=PROP`
//! names nodes by a property; without it Neptune's `~id` string
//! names the node.
//!
//! Every request is **SigV4-signed** (service `neptune-db`)
//! through the shared `quarb-aws` chain — the same credentials
//! that sign your S3 and DynamoDB mounts. The label catalog
//! prefers the summary API
//! (`GET /propertygraph/statistics/summary`) and falls back to a
//! `DISTINCT labels(n)` sweep where the summary is disabled.
//!
//! Neptune is reachable only inside its VPC; from a workload
//! there, `qua` and this crate work as anywhere else. Offline,
//! the protocol is held honest by a bottled endpoint in the
//! integration tests.
//!
//! **Target**:
//! `neptune://HOST[:8182][?region=…&key=PROP&endpoint=URL]` —
//! `endpoint=` overrides scheme+host (the bottle, a proxy);
//! region from the standard chain when not given.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading Neptune.
#[derive(Debug, thiserror::Error)]
pub enum NeptuneError {
    #[error("neptune: {0}")]
    Api(String),
    #[error("neptune target: {0} (expected neptune://HOST[:8182][?region=…&key=PROP])")]
    Target(String),
    #[error("neptune: no credentials in the chain (set AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or ~/.aws/credentials)")]
    NoCredentials,
}

/// A JSON value as a Quarb value (scalar cells).
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

/// A value as an inline Cypher literal (the endpoint takes whole
/// statements; strings escape their quotes).
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
    /// A graph node: its `~id`, labels, and decoded properties.
    Entity {
        nid: String,
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

/// A Neptune graph, exposed as an arbor.
pub struct NeptuneAdapter {
    creds: quarb_aws::Credentials,
    region: String,
    /// `https://HOST:PORT` (or the `endpoint=` override).
    base: String,
    key: Vec<String>,
    labels: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    by_nid: RefCell<HashMap<String, NodeId>>,
    edge_props: RefCell<HashMap<(NodeId, String, NodeId), Vec<(String, Value)>>>,
}

impl NeptuneAdapter {
    /// Connect to `neptune://…`; the label catalog doubles as the
    /// probe.
    pub fn connect(target: &str) -> Result<Self, NeptuneError> {
        let rest = target
            .strip_prefix("neptune://")
            .ok_or_else(|| NeptuneError::Target(target.to_string()))?;
        let (hostport, query) = match rest.split_once('?') {
            Some((h, q)) => (h, Some(q)),
            None => (rest, None),
        };
        let hostport = hostport.trim_end_matches('/');
        let param = |k: &str| {
            query.and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            })
        };
        let base = match param("endpoint") {
            Some(e) => e.trim_end_matches('/').to_string(),
            None => {
                if hostport.is_empty() {
                    return Err(NeptuneError::Target(target.to_string()));
                }
                let hp = if hostport.contains(':') {
                    hostport.to_string()
                } else {
                    format!("{hostport}:8182")
                };
                format!("https://{hp}")
            }
        };
        let region = quarb_aws::region(param("region").as_deref());
        let creds = quarb_aws::load_credentials().ok_or(NeptuneError::NoCredentials)?;
        let adapter = NeptuneAdapter {
            creds,
            region,
            base,
            key: param("key")
                .map(|k| k.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
            labels: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_nid: RefCell::new(HashMap::new()),
            edge_props: RefCell::new(HashMap::new()),
        };
        // Catalog: the summary API when enabled, else a sweep.
        let mut labels = adapter.summary_labels().unwrap_or_default();
        if labels.is_empty() {
            labels = adapter
                .cypher("MATCH (n) UNWIND labels(n) AS l RETURN DISTINCT l ORDER BY l")?
                .iter()
                .filter_map(|row| row.pointer("/l").and_then(|v| v.as_str().map(str::to_string)))
                .collect();
        }
        labels.sort();
        labels.dedup();
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

    /// A signed request; POST bodies are the openCypher form.
    fn signed(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        content_type: Option<&str>,
    ) -> Result<Json, NeptuneError> {
        let extra: Vec<(&str, &str)> = content_type
            .map(|ct| vec![("content-type", ct)])
            .unwrap_or_default();
        let headers = quarb_aws::sign(
            &self.creds,
            method,
            url,
            &self.region,
            "neptune-db",
            body,
            &extra,
        );
        let mut req = ureq::request(method, url);
        for (k, v) in &headers {
            if k != "host" {
                req = req.set(k, v);
            }
        }
        let resp = if body.is_empty() {
            req.call()
        } else {
            req.send_bytes(body)
        };
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<Json>(&text)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/detailedMessage")
                            .or_else(|| v.pointer("/message"))
                            .and_then(|m| m.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or(text);
                return Err(NeptuneError::Api(format!("{code}: {msg}")));
            }
            Err(e) => return Err(NeptuneError::Api(e.to_string())),
        };
        resp.into_json()
            .map_err(|e| NeptuneError::Api(format!("decoding response: {e}")))
    }

    /// Labels from `GET /propertygraph/statistics/summary`, when
    /// the summary API is enabled.
    fn summary_labels(&self) -> Option<Vec<String>> {
        let url = format!("{}/propertygraph/statistics/summary", self.base);
        let json = self.signed("GET", &url, &[], None).ok()?;
        Some(
            json.pointer("/payload/graphSummary/nodeLabels")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
    }

    /// One openCypher statement; rows come back as alias-keyed
    /// JSON objects.
    fn cypher(&self, stmt: &str) -> Result<Vec<Json>, NeptuneError> {
        let url = format!("{}/openCypher", self.base);
        let body = format!("query={}", urlencode(stmt));
        let json = self.signed(
            "POST",
            &url,
            body.as_bytes(),
            Some("application/x-www-form-urlencoded"),
        )?;
        Ok(json
            .pointer("/results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Intern one vertex object (`~id`/`~labels`/`~properties`).
    fn intern(&self, j: &Json) -> Option<NodeId> {
        let nid = j.pointer("/~id")?.as_str()?.to_string();
        if let Some(&id) = self.by_nid.borrow().get(&nid) {
            return Some(id);
        }
        let labels: Vec<String> = j
            .pointer("/~labels")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let props: Vec<(String, Value)> = j
            .pointer("/~properties")
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
            .unwrap_or_else(|| nid.clone());
        let parent = labels
            .first()
            .and_then(|l| self.labels.iter().position(|x| x == l))
            .map(|i| NodeId(i as u64 + 1));
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Entity {
                nid: nid.clone(),
                labels,
                props,
            },
            name: Some(name),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_nid.borrow_mut().insert(nid, id);
        Some(id)
    }

    /// One node's relationships, outgoing or incoming.
    fn edges(&self, node: NodeId, incoming: bool) -> Vec<(String, NodeId)> {
        let nid = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entity { nid, .. } => nid.clone(),
            _ => return Vec::new(),
        };
        let arrow = if incoming { "<-[r]-" } else { "-[r]->" };
        let stmt = format!(
            "MATCH (n){arrow}(m) WHERE id(n) = {} RETURN r, m ORDER BY type(r), id(m)",
            cypher_literal(&Value::Str(nid))
        );
        let rows = self.cypher(&stmt).unwrap_or_default();
        rows.iter()
            .filter_map(|row| {
                let edge = row.pointer("/r")?;
                let vert = row.pointer("/m")?;
                let label = edge.pointer("/~type")?.as_str()?.to_string();
                let other = self.intern(vert)?;
                let (source, target) = if incoming { (other, node) } else { (node, other) };
                let props: Vec<(String, Value)> = edge
                    .pointer("/~properties")
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

/// Percent-encode a query string body value.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl AstAdapter for NeptuneAdapter {
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
            format!("coalesce(m.`{}`, id(m))", self.key.join("`, m.`"))
        };
        let stmt = format!("MATCH (m:`{label}`) RETURN m ORDER BY {order}");
        let rows = self.cypher(&stmt).unwrap_or_default();
        let ids: Vec<NodeId> = rows
            .iter()
            .filter_map(|row| self.intern(row.pointer("/m")?))
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
            (Kind::Label { index }, "n-rows") => {
                let stmt = format!(
                    "MATCH (m:`{}`) RETURN count(m) AS c",
                    self.labels[*index]
                );
                drop(nodes);
                let rows = self.cypher(&stmt).ok()?;
                rows.first()?.pointer("/c").map(cell_value)
            }
            (Kind::Entity { nid, .. }, "id") => Some(Value::Str(nid.clone())),
            (Kind::Entity { labels, .. }, "labels") => Some(Value::List(
                labels.iter().map(|l| Value::Str(l.clone())).collect(),
            )),
            _ => None,
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, false)
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.edges(node, true)
    }

    /// Hint-form resolution against the target label's `?key=`
    /// property (or `~id` without one).
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let label = hint?;
        let value = self.property(node, property)?;
        let cond = match self.key.first() {
            Some(k) => format!("m.`{k}` = {}", cypher_literal(&value)),
            None => format!("id(m) = {}", cypher_literal(&value)),
        };
        let stmt = format!("MATCH (m:`{label}`) WHERE {cond} RETURN m LIMIT 1");
        let rows = self.cypher(&stmt).ok()?;
        self.intern(rows.first()?.pointer("/m")?)
    }

    /// `$-::prop` — a relationship's own property, from the cache
    /// the edge fetch filled; a cold read refetches the source's
    /// edges.
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
    fn encodes() {
        assert_eq!(urlencode("a b&c=1"), "a+b%26c%3D1");
        assert_eq!(urlencode("MATCH (n)"), "MATCH+%28n%29");
    }

    #[test]
    fn literals() {
        assert_eq!(cypher_literal(&Value::Int(7)), "7");
        assert_eq!(cypher_literal(&Value::Str("O'Slo".into())), r"'O\'Slo'");
    }
}
