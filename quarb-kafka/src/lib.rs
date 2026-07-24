//! Apache Kafka adapter for the Quarb query engine.
//!
//! Topics at the root, a topic's messages as its children, and
//! each message's payload below it — JSON payloads decode into
//! attribute subtrees (the dual-exposure doctrine applies), plain
//! text stays a leaf value.
//!
//! **Bounding.** A mount is a *bounded snapshot*, never a tail:
//! each partition's high watermark, read when the topic is first
//! touched, is the implicit upper bound, so a query always runs
//! over a finite, ordered window. `?from=` / `?until=` narrow the
//! window by record timestamp (ISO-8601 instants or epoch
//! milliseconds; `from` inclusive, `until` exclusive). Within the
//! window, messages order deterministically by (timestamp,
//! partition, offset) — the source may of course move between
//! mounts, exactly as any live adapter's source may.
//!
//! **Naming.** Messages are named by their record *key* — the
//! entity id in keyed topics — and names repeat, exactly as
//! `/row` repeats in CSV: `/users/'42'` is the history of entity
//! 42 in the topic, `/users/'42'[-1]` its latest state. Keyless
//! messages are unnamed (positional access still works).
//!
//! **References.** Kafka's table-stream duality makes resolve a
//! stream-table join: `::user_id~>users` finds the *latest*
//! message in the `users` topic whose key equals the value —
//! against a compacted topic, that is precisely the entity's
//! current row. Hint-less resolution tries the property name
//! minus a trailing `_id`, pluralized bare.
//!
//! **Metadata**: `;;;key`, `;;;partition`, `;;;offset`, `;;;ts`
//! (a typed instant — `[;;;ts > 2026-07-01]` works), `;;;topic`,
//! and any record header by its own name; `;;;partitions` on a
//! topic. Internal topics (`__consumer_offsets` &c.) stay hidden
//! unless `?internal=1`.
//!
//! **Target**:
//! `kafka://[USER:PASS@]HOST:PORT[,HOST:PORT…][?topics=a,b&from=…&until=…&internal=1&tls=1&sasl=…]`
//! — read-only, as always. Credentials in the target turn on
//! SASL (`sasl=` picks the mechanism: `scram-sha-256` (the
//! default), `scram-sha-512` (Amazon MSK's choice), `plain`);
//! `tls=1` wraps the connection in TLS against the standard
//! roots — managed clusters (MSK public access, Aiven, Redpanda
//! Cloud) want both. Percent-escapes in USER/PASS decode.

use quarb::{AstAdapter, NodeId, Value};
use rskafka::client::partition::{OffsetAt, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use std::cell::RefCell;

/// An error connecting to or reading Kafka.
#[derive(Debug, thiserror::Error)]
pub enum KafkaError {
    #[error("kafka: {0}")]
    Api(String),
    #[error("kafka target: {0} (expected kafka://HOST:PORT[,HOST:PORT…][?topics=…&from=…&until=…])")]
    Target(String),
}

/// One decoded payload node: a JSON attribute tree or a scalar
/// leaf.
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

/// One fetched record, decoded.
#[derive(Clone)]
struct Msg {
    partition: i32,
    offset: i64,
    secs: i64,
    nanos: u32,
    key: Option<String>,
    headers: Vec<(String, String)>,
    payload: Field,
}

enum Kind {
    Root,
    Topic {
        name: String,
        partitions: Vec<i32>,
    },
    Message {
        topic: String,
        msg: Msg,
    },
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

/// A Kafka cluster's topics, exposed as an arbor of bounded
/// message windows.
pub struct KafkaAdapter {
    rt: tokio::runtime::Runtime,
    client: Client,
    /// Window bounds by record timestamp, if narrowed.
    from: Option<(i64, u32)>,
    until: Option<(i64, u32)>,
    nodes: RefCell<Vec<Node>>,
}

/// Decode `%XX` escapes in a target's userinfo part.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `2026-07-24T12:00:00Z`-style instants or bare epoch millis.
fn parse_bound(s: &str) -> Option<(i64, u32)> {
    if let Some((secs, nanos, _)) = quarb::temporal::parse_iso(s) {
        return Some((secs, nanos));
    }
    let ms: i64 = s.parse().ok()?;
    Some((ms.div_euclid(1000), (ms.rem_euclid(1000) as u32) * 1_000_000))
}

impl KafkaAdapter {
    /// Connect to `kafka://…`; one metadata round-trip lists the
    /// topics, so a bad broker fails at connect, not mid-query.
    pub fn connect(target: &str) -> Result<Self, KafkaError> {
        let rest = target
            .strip_prefix("kafka://")
            .or_else(|| target.strip_prefix("kafka:"))
            .ok_or_else(|| KafkaError::Target(target.to_string()))?;
        let (brokers, query) = match rest.split_once('?') {
            Some((b, q)) => (b, Some(q)),
            None => (rest, None),
        };
        let mut brokers = brokers.trim_end_matches('/');
        // Userinfo turns on SASL; the mechanism comes from `sasl=`.
        let creds = brokers.rsplit_once('@').and_then(|(userinfo, hosts)| {
            brokers = hosts;
            let (user, pass) = userinfo.split_once(':')?;
            Some((percent_decode(user), percent_decode(pass)))
        });
        if brokers.is_empty() {
            return Err(KafkaError::Target(target.to_string()));
        }
        let param = |k: &str| {
            query.and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            })
        };
        let bound = |k: &str| -> Result<Option<(i64, u32)>, KafkaError> {
            match param(k) {
                None => Ok(None),
                Some(s) => parse_bound(&s).map(Some).ok_or_else(|| {
                    KafkaError::Target(format!("{k}={s}: not an instant or epoch millis"))
                }),
            }
        };
        let from = bound("from")?;
        let until = bound("until")?;
        let wanted: Option<Vec<String>> =
            param("topics").map(|t| t.split(',').map(str::to_string).collect());
        let internal = param("internal").is_some_and(|v| v != "0");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| KafkaError::Api(e.to_string()))?;
        let hosts: Vec<String> = brokers.split(',').map(str::to_string).collect();
        // A finite retry deadline: the library default retries
        // forever, turning a bad password or listener into a hang.
        let mut builder = ClientBuilder::new(hosts).backoff_config(rskafka::BackoffConfig {
            deadline: Some(std::time::Duration::from_secs(30)),
            ..Default::default()
        });
        if let Some((user, pass)) = creds {
            use rskafka::client::{Credentials, SaslConfig};
            let c = Credentials::new(user, pass);
            let mech = param("sasl").unwrap_or_else(|| "scram-sha-256".into());
            builder = builder.sasl_config(match mech.as_str() {
                "scram-sha-256" => SaslConfig::ScramSha256(c),
                "scram-sha-512" => SaslConfig::ScramSha512(c),
                "plain" => SaslConfig::Plain(c),
                other => {
                    return Err(KafkaError::Target(format!(
                        "sasl={other}: expected scram-sha-256, scram-sha-512, or plain"
                    )));
                }
            });
        }
        if param("tls").is_some_and(|v| v != "0") {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|e| KafkaError::Api(format!("tls: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
            builder = builder.tls_config(std::sync::Arc::new(tls));
        }
        let (client, mut topics) = rt
            .block_on(async {
                let client = builder.build().await?;
                let topics = client.list_topics().await?;
                Ok::<_, rskafka::client::error::Error>((client, topics))
            })
            .map_err(|e| KafkaError::Api(e.to_string()))?;
        topics.retain(|t| {
            (internal || !t.name.starts_with("__"))
                && wanted.as_ref().is_none_or(|w| w.contains(&t.name))
        });
        topics.sort_by(|a, b| a.name.cmp(&b.name));
        let adapter = KafkaAdapter {
            rt,
            client,
            from,
            until,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        let ids: Vec<NodeId> = topics
            .into_iter()
            .map(|t| {
                adapter.push(Node {
                    kind: Kind::Topic {
                        name: t.name.clone(),
                        partitions: t.partitions.iter().copied().collect(),
                    },
                    name: Some(t.name),
                    parent: Some(NodeId(0)),
                    children: RefCell::new(None),
                })
            })
            .collect();
        *adapter.nodes.borrow()[0].children.borrow_mut() = Some(ids);
        Ok(adapter)
    }

    /// A human-readable locator: `topic/key…`, with unnamed
    /// (keyless) messages shown by coordinate.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let nodes = self.nodes.borrow();
            let n = &nodes[id.0 as usize];
            match (&n.name, &n.kind) {
                (Some(name), _) => parts.push(name.clone()),
                (None, Kind::Message { msg, .. }) => {
                    parts.push(format!("@p{}:{}", msg.partition, msg.offset));
                }
                _ => {}
            }
            cur = n.parent;
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

    /// Materialize a container's children, lazily. The plan is
    /// extracted first so the arena borrow is released before
    /// any child pushes re-borrow it mutably.
    fn kids_of(&self, node: NodeId) -> Vec<NodeId> {
        enum Plan {
            Done(Vec<NodeId>),
            Leaf,
            Topic(String, Vec<i32>),
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
                    Kind::Topic { name, partitions } => {
                        Plan::Topic(name.clone(), partitions.clone())
                    }
                    Kind::Message { msg, .. } => match &msg.payload {
                        Field::Map(entries) => Plan::Entries(entries.clone()),
                        Field::List(items) => Plan::List(items.clone()),
                        Field::Scalar(_) => Plan::Leaf,
                    },
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
            Plan::Topic(name, parts) => self
                .fetch_topic(&name, &parts)
                .unwrap_or_default()
                .into_iter()
                .map(|msg| {
                    self.push(Node {
                        name: msg.key.clone(),
                        kind: Kind::Message {
                            topic: name.clone(),
                            msg,
                        },
                        parent: Some(node),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Plan::Entries(entries) => entries
                .into_iter()
                .map(|(k, f)| {
                    self.push(Node {
                        kind: Kind::Field { value: f },
                        name: Some(k),
                        parent: Some(node),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
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

    /// One bounded read of a topic: every partition from its
    /// earliest retained offset to its high watermark as of this
    /// call — the snapshot bound — then the window filter and the
    /// deterministic (timestamp, partition, offset) sort.
    fn fetch_topic(&self, topic: &str, partitions: &[i32]) -> Result<Vec<Msg>, KafkaError> {
        let mut msgs = Vec::new();
        for &p in partitions {
            let records = self
                .rt
                .block_on(async {
                    let pc = self
                        .client
                        .partition_client(topic, p, UnknownTopicHandling::Error)
                        .await?;
                    let start = pc.get_offset(OffsetAt::Earliest).await?;
                    let watermark = pc.get_offset(OffsetAt::Latest).await?;
                    let mut out = Vec::new();
                    let mut cur = start;
                    while cur < watermark {
                        let (batch, _) =
                            pc.fetch_records(cur, 1..8 * 1024 * 1024, 500).await?;
                        if batch.is_empty() {
                            break;
                        }
                        for r in batch {
                            cur = cur.max(r.offset + 1);
                            if r.offset < watermark {
                                out.push(r);
                            }
                        }
                    }
                    Ok::<_, rskafka::client::error::Error>(out)
                })
                .map_err(|e| KafkaError::Api(format!("{topic}/{p}: {e}")))?;
            for r in records {
                let rec = r.record;
                let secs = rec.timestamp.timestamp();
                let nanos = rec.timestamp.timestamp_subsec_nanos();
                if let Some(f) = self.from
                    && (secs, nanos) < f
                {
                    continue;
                }
                if let Some(u) = self.until
                    && (secs, nanos) >= u
                {
                    continue;
                }
                msgs.push(Msg {
                    partition: p,
                    offset: r.offset,
                    secs,
                    nanos,
                    key: rec
                        .key
                        .as_deref()
                        .and_then(|k| std::str::from_utf8(k).ok().map(str::to_string)),
                    headers: rec
                        .headers
                        .iter()
                        .map(|(k, v)| (k.clone(), String::from_utf8_lossy(v).into_owned()))
                        .collect(),
                    payload: decode_payload(rec.value.as_deref()),
                });
            }
        }
        msgs.sort_by_key(|m| (m.secs, m.nanos, m.partition, m.offset));
        Ok(msgs)
    }
}

/// A payload is a JSON attribute tree when it parses as JSON, a
/// text leaf when it is UTF-8, and null otherwise.
fn decode_payload(value: Option<&[u8]>) -> Field {
    let Some(bytes) = value else {
        return Field::Scalar(Value::Null);
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Field::Scalar(Value::Null);
    };
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

impl AstAdapter for KafkaAdapter {
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
            Kind::Topic { .. } => vec!["topic".into()],
            Kind::Message { .. } => vec!["message".into()],
            Kind::Field { .. } => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let entries = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Message { msg, .. } => match &msg.payload {
                Field::Map(entries) => entries.clone(),
                _ => return None,
            },
            Kind::Field {
                value: Field::Map(entries),
            } => entries.clone(),
            _ => return None,
        };
        entries
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, f)| f.scalar())
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Message { msg, .. } => msg.payload.scalar(),
            Kind::Field { value } => value.scalar(),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Topic { partitions, .. } => match key {
                "partitions" => Some(Value::Int(partitions.len() as i64)),
                _ => None,
            },
            Kind::Message { topic, msg } => match key {
                "key" => msg.key.clone().map(Value::Str),
                "partition" => Some(Value::Int(msg.partition as i64)),
                "offset" => Some(Value::Int(msg.offset)),
                "topic" => Some(Value::Str(topic.clone())),
                "ts" => Some(Value::Instant {
                    secs: msg.secs,
                    nanos: msg.nanos,
                    offset_min: None,
                }),
                other => msg
                    .headers
                    .iter()
                    .find(|(k, _)| k == other)
                    .map(|(_, v)| Value::Str(v.clone())),
            },
            _ => None,
        }
    }

    /// The stream-table join: the latest message in the target
    /// topic whose key equals the property's value. Against a
    /// compacted topic this is the entity's current state.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let value = self.property(node, property)?.to_string();
        let candidates: Vec<String> = match hint {
            Some(h) => vec![h.to_string()],
            None => {
                let stem = property.strip_suffix("_id").unwrap_or(property);
                vec![format!("{stem}s"), stem.to_string()]
            }
        };
        let topics: Vec<(String, NodeId)> = {
            let nodes = self.nodes.borrow();
            let root_kids = nodes[0].children.borrow().clone().unwrap_or_default();
            root_kids
                .iter()
                .filter_map(|id| nodes[id.0 as usize].name.clone().map(|n| (n, *id)))
                .collect()
        };
        for c in candidates {
            let Some((_, tid)) = topics.iter().find(|(n, _)| n == &c) else {
                continue;
            };
            let kids = self.kids_of(*tid);
            let nodes = self.nodes.borrow();
            if let Some(hit) = kids
                .iter()
                .rev()
                .find(|id| nodes[id.0 as usize].name.as_deref() == Some(&value))
            {
                return Some(*hit);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_parse() {
        // ISO instants and epoch millis both work; garbage fails.
        assert_eq!(
            parse_bound("2026-07-24T12:00:00Z"),
            quarb::temporal::parse_iso("2026-07-24T12:00:00Z").map(|(s, n, _)| (s, n))
        );
        assert_eq!(parse_bound("1753358400123"), Some((1753358400, 123_000_000)));
        assert_eq!(parse_bound("half past nine"), None);
    }

    #[test]
    fn userinfo_decoding() {
        assert_eq!(percent_decode("p%40ss%3Aword"), "p@ss:word");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("trailing%4"), "trailing%4");
    }

    #[test]
    fn payload_sniffing() {
        // JSON decodes to a tree, text stays a leaf, binary is null.
        assert!(matches!(
            decode_payload(Some(br#"{"a": 1}"#)),
            Field::Map(ref e) if e.len() == 1
        ));
        assert!(matches!(
            decode_payload(Some(b"plain text")),
            Field::Scalar(Value::Str(ref s)) if s == "plain text"
        ));
        assert!(matches!(
            decode_payload(Some(b"{not json")),
            Field::Scalar(Value::Str(_))
        ));
        assert!(matches!(
            decode_payload(Some(&[0xff, 0xfe])),
            Field::Scalar(Value::Null)
        ));
        assert!(matches!(
            decode_payload(None),
            Field::Scalar(Value::Null)
        ));
    }
}
