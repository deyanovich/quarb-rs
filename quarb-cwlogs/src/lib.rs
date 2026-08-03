//! AWS CloudWatch Logs adapter for the Quarb query engine.
//!
//! The gcplogs design sheet, over AWS: a mount is a *bounded
//! snapshot* — `since=` and/or `limit=` are mandatory, an
//! unbounded target is refused — and log events become an arbor.
//! With a group named in the target, its events sit at the root
//! as `/entry` children, oldest first; without one, the root
//! holds a child per log group (short name = the path's last
//! segment), each group's events fetched lazily on first touch.
//!
//! CloudWatch's envelope is thinner than GCP's — no severity, no
//! trace — so the payload matters more: a JSON `message` decodes
//! into an attribute subtree with property fallthrough, which
//! makes `::level`, `::requestId`, or whatever your services log
//! addressable as plain properties, and any shared field a
//! `<=>` join key across groups.
//!
//! - **Properties**: `::timestamp` (a typed instant),
//!   `::logStream`; everything else falls through to the decoded
//!   payload's top level.
//! - **The default value** (`::`) is the raw message for text
//!   events, or the `message`/`logMessage` convention inside
//!   JSON payloads.
//! - **Metadata**: `;;;group` (the full log-group name),
//!   `;;;stream`, `;;;received` (ingestion instant), `;;;id`
//!   (the event id).
//!
//! **Transport and auth**: SigV4-signed calls to the
//! `Logs_20140328` JSON protocol via the shared `quarb-aws`
//! plumbing — the credential chain (env, `~/.aws/credentials`)
//! and region resolution behave as every other AWS adapter here.
//! `endpoint=` overrides the URL (LocalStack, and the bottled
//! test server).
//!
//! **Target**:
//! `cwl:[GROUP]?since=1h&until=…&filter=…&limit=N&region=…&endpoint=…`
//! (`cloudwatch:` works too). `since` takes a duration (`30m`,
//! `1h`, `2d`) or an ISO instant; `filter` is a CloudWatch
//! filter pattern, applied server-side.

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use serde_json::json;
use std::cell::RefCell;

/// An error connecting to or reading CloudWatch Logs.
#[derive(Debug, thiserror::Error)]
pub enum CwlError {
    #[error("cloudwatch: {0}")]
    Api(String),
    #[error(
        "cwlogs target: {0} (expected cwl:[GROUP]?since=1h&filter=…&limit=N; \
         a mount must be bounded — give it since= and/or limit=)"
    )]
    Target(String),
}

/// One decoded payload node: a JSON attribute tree or a scalar.
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

/// One log event, decoded.
#[derive(Clone)]
struct Event {
    secs: i64,
    nanos: u32,
    received: Option<(i64, u32)>,
    stream: Option<String>,
    id: Option<String>,
    /// The raw message when it is not JSON.
    text: Option<String>,
    /// The JSON message as a field tree.
    payload: Option<Field>,
}

enum Kind {
    Root,
    /// A log group (full name, short node name), events lazy.
    Group(String),
    Event { group: usize, idx: usize },
    Field(Field),
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A bounded snapshot of CloudWatch log events.
pub struct CwlAdapter {
    t: Target,
    creds: quarb_aws::Credentials,
    region: String,
    endpoint: String,
    /// Full group names, indexed by the `Kind::Event.group` slot;
    /// events cached per group after the first fetch.
    groups: RefCell<Vec<(String, Vec<Event>)>>,
    nodes: RefCell<Vec<Node>>,
}

struct Target {
    group: Option<String>,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    filter: Option<String>,
    limit: Option<u64>,
}

/// A duration suffix: `30m`, `1h`, `2d`.
fn duration_ms(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * mult)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
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

/// Parse the target; `region`/`endpoint` come back separately so
/// the pure part stays testable.
fn parse_target(target: &str) -> Result<(Target, Option<String>, Option<String>), CwlError> {
    let rest = target
        .strip_prefix("cwl:")
        .or_else(|| target.strip_prefix("cloudwatch:"))
        .ok_or_else(|| CwlError::Target(target.to_string()))?;
    let rest = rest.trim_start_matches("//");
    let (group, query) = match rest.split_once('?') {
        Some((g, q)) => (g, Some(q)),
        None => (rest, None),
    };
    let param = |k: &str| {
        query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(percent_decode))
        })
    };
    // The pinned invocation instant resolves `since=` (see
    // quarb::now_secs) — a pinned run's window replays.
    let now_ms = quarb::now_secs() * 1000;
    let since_ms = match param("since") {
        None => None,
        Some(s) => Some(if let Some(d) = duration_ms(&s) {
            now_ms - d
        } else if let Some((secs, nanos, _)) = parse_iso(&s) {
            secs * 1000 + (nanos / 1_000_000) as i64
        } else {
            return Err(CwlError::Target(format!(
                "since={s}: not a duration (30m, 1h, 2d) or an ISO instant"
            )));
        }),
    };
    let until_ms = match param("until") {
        None => None,
        Some(u) => match parse_iso(&u) {
            Some((secs, nanos, _)) => Some(secs * 1000 + (nanos / 1_000_000) as i64),
            None => return Err(CwlError::Target(format!("until={u}: not an ISO instant"))),
        },
    };
    let limit = match param("limit") {
        None => None,
        Some(l) => Some(
            l.parse::<u64>()
                .map_err(|_| CwlError::Target(format!("limit={l}: not a number")))?,
        ),
    };
    if since_ms.is_none() && limit.is_none() {
        return Err(CwlError::Target(format!(
            "{target}: unbounded — a busy group holds more events than \
             you want to snapshot"
        )));
    }
    Ok((
        Target {
            group: (!group.is_empty()).then(|| group.to_string()),
            since_ms,
            until_ms,
            filter: param("filter"),
            limit,
        },
        param("region"),
        param("endpoint"),
    ))
}

fn decode_json(v: &Json) -> Field {
    match v {
        Json::Null => Field::Scalar(Value::Null),
        Json::Bool(b) => Field::Scalar(Value::Bool(*b)),
        Json::Number(n) => Field::Scalar(match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        }),
        Json::String(s) => Field::Scalar(str_value(s)),
        Json::Array(a) => Field::List(a.iter().map(decode_json).collect()),
        Json::Object(o) => Field::Map(o.iter().map(|(k, v)| (k.clone(), decode_json(v))).collect()),
    }
}

/// Full RFC 3339 timestamps become instants; other strings stay
/// strings.
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

/// Decode one FilterLogEvents event.
fn decode_event(o: &Json) -> Option<Event> {
    let ms = o.pointer("/timestamp")?.as_i64()?;
    let message = o.pointer("/message").and_then(|v| v.as_str()).unwrap_or("");
    let trimmed = message.trim();
    let (text, payload) = if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(v) = serde_json::from_str::<Json>(trimmed)
    {
        (None, Some(decode_json(&v)))
    } else {
        (Some(message.trim_end().to_string()), None)
    };
    Some(Event {
        secs: ms.div_euclid(1000),
        nanos: (ms.rem_euclid(1000) as u32) * 1_000_000,
        received: o.pointer("/ingestionTime").and_then(|v| v.as_i64()).map(|ms| {
            (ms.div_euclid(1000), (ms.rem_euclid(1000) as u32) * 1_000_000)
        }),
        stream: o
            .pointer("/logStreamName")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        id: o
            .pointer("/eventId")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        text,
        payload,
    })
}

/// A group's short node name: the last path segment
/// (`/aws/lambda/checkout` → `checkout`).
fn short_group(full: &str) -> String {
    full.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(full)
        .to_string()
}

impl CwlAdapter {
    /// Open a bounded snapshot of `target`'s log events.
    pub fn open(target: &str) -> Result<Self, CwlError> {
        let (t, region, endpoint) = parse_target(target)?;
        let creds = quarb_aws::load_credentials().ok_or_else(|| {
            CwlError::Api(
                "no AWS credentials (env AWS_ACCESS_KEY_ID/… or ~/.aws/credentials)".into(),
            )
        })?;
        let region = quarb_aws::region(region.as_deref());
        let endpoint =
            endpoint.unwrap_or_else(|| format!("https://logs.{region}.amazonaws.com/"));
        let adapter = CwlAdapter {
            t,
            creds,
            region,
            endpoint,
            groups: RefCell::new(Vec::new()),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        adapter.seed()?;
        Ok(adapter)
    }

    /// One signed `Logs_20140328` call.
    fn call(&self, action: &str, body: &Json) -> Result<Json, CwlError> {
        let payload = serde_json::to_vec(body).map_err(|e| CwlError::Api(e.to_string()))?;
        let target = format!("Logs_20140328.{action}");
        let extra = [
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target.as_str()),
        ];
        let headers = quarb_aws::sign(
            &self.creds,
            "POST",
            &self.endpoint,
            &self.region,
            "logs",
            &payload,
            &extra,
        );
        let mut req = ureq::post(&self.endpoint);
        for (k, v) in &headers {
            if k != "host" {
                req = req.set(k, v);
            }
        }
        let resp = req
            .send_bytes(&payload)
            .map_err(|e| CwlError::Api(format!("{action}: {e}")))?;
        let text = resp
            .into_string()
            .map_err(|e| CwlError::Api(format!("{action}: {e}")))?;
        serde_json::from_str(&text).map_err(|e| CwlError::Api(format!("{action}: {e}")))
    }

    /// Fill the root: the named group's events, or one child per
    /// discovered group (events lazy).
    fn seed(&self) -> Result<(), CwlError> {
        let root_kids: Vec<NodeId> = match self.t.group.clone() {
            Some(full) => {
                let events = self.fetch_events(&full)?;
                self.groups.borrow_mut().push((full, events));
                let n = self.groups.borrow()[0].1.len();
                (0..n)
                    .map(|idx| {
                        self.push(Node {
                            kind: Kind::Event { group: 0, idx },
                            name: Some("entry".into()),
                            parent: Some(NodeId(0)),
                            children: RefCell::new(None),
                        })
                    })
                    .collect()
            }
            None => {
                let mut names: Vec<String> = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut body = json!({});
                    if let Some(tk) = &token {
                        body["nextToken"] = json!(tk);
                    }
                    let resp = self.call("DescribeLogGroups", &body)?;
                    for g in resp
                        .pointer("/logGroups")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                    {
                        if let Some(n) = g.pointer("/logGroupName").and_then(|v| v.as_str()) {
                            names.push(n.to_string());
                        }
                    }
                    token = resp
                        .pointer("/nextToken")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    if token.is_none() {
                        break;
                    }
                }
                names.sort();
                names
                    .into_iter()
                    .map(|full| {
                        let short = short_group(&full);
                        self.push(Node {
                            kind: Kind::Group(full),
                            name: Some(short),
                            parent: Some(NodeId(0)),
                            children: RefCell::new(None),
                        })
                    })
                    .collect()
            }
        };
        *self.nodes.borrow()[0].children.borrow_mut() = Some(root_kids);
        Ok(())
    }

    /// One bounded FilterLogEvents sweep of a group, oldest first.
    fn fetch_events(&self, group: &str) -> Result<Vec<Event>, CwlError> {
        let mut events: Vec<Event> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({ "logGroupName": group });
            if let Some(s) = self.t.since_ms {
                body["startTime"] = json!(s);
            }
            if let Some(u) = self.t.until_ms {
                body["endTime"] = json!(u);
            }
            if let Some(f) = &self.t.filter {
                body["filterPattern"] = json!(f);
            }
            if let Some(l) = self.t.limit {
                let want = l - (events.len() as u64).min(l);
                body["limit"] = json!(want.clamp(1, 10_000));
            }
            if let Some(tk) = &token {
                body["nextToken"] = json!(tk);
            }
            let resp = self.call("FilterLogEvents", &body)?;
            for e in resp
                .pointer("/events")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(ev) = decode_event(e) {
                    events.push(ev);
                }
            }
            if let Some(l) = self.t.limit
                && events.len() as u64 >= l
            {
                events.truncate(l as usize);
                break;
            }
            token = resp
                .pointer("/nextToken")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        events.sort_by(|a, b| (a.secs, a.nanos, &a.id).cmp(&(b.secs, b.nanos, &b.id)));
        Ok(events)
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// A human-readable locator: `[group/]entry[N]/…`.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let nodes = self.nodes.borrow();
            let n = &nodes[id.0 as usize];
            match &n.kind {
                Kind::Root => {}
                Kind::Event { idx, .. } => parts.push(format!("entry[{}]", idx + 1)),
                _ => parts.push(n.name.clone().unwrap_or_default()),
            }
            cur = n.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn kids_of(&self, node: NodeId) -> Vec<NodeId> {
        enum Plan {
            Done(Vec<NodeId>),
            Leaf,
            Group(String),
            Event(usize, usize),
            Fields(Vec<(String, Field)>),
            Items(Vec<Field>),
        }
        let plan = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            if let Some(k) = &*n.children.borrow() {
                Plan::Done(k.clone())
            } else {
                match &n.kind {
                    Kind::Root => Plan::Leaf,
                    Kind::Group(full) => Plan::Group(full.clone()),
                    Kind::Event { group, idx } => Plan::Event(*group, *idx),
                    Kind::Field(Field::Map(entries)) => Plan::Fields(entries.clone()),
                    Kind::Field(Field::List(items)) => Plan::Items(items.clone()),
                    Kind::Field(Field::Scalar(_)) => Plan::Leaf,
                }
            }
        };
        let made = match plan {
            Plan::Done(k) => return k,
            Plan::Leaf => Vec::new(),
            Plan::Group(full) => {
                let events = self.fetch_events(&full).unwrap_or_default();
                let gi = {
                    let mut groups = self.groups.borrow_mut();
                    groups.push((full, events));
                    groups.len() - 1
                };
                let n = self.groups.borrow()[gi].1.len();
                (0..n)
                    .map(|idx| {
                        self.push(Node {
                            kind: Kind::Event { group: gi, idx },
                            name: Some("entry".into()),
                            parent: Some(node),
                            children: RefCell::new(None),
                        })
                    })
                    .collect()
            }
            Plan::Event(g, i) => {
                let fields = match &self.groups.borrow()[g].1[i].payload {
                    Some(Field::Map(entries)) => entries.clone(),
                    _ => Vec::new(),
                };
                fields
                    .into_iter()
                    .map(|(k, f)| {
                        self.push(Node {
                            kind: Kind::Field(f),
                            name: Some(k),
                            parent: Some(node),
                            children: RefCell::new(None),
                        })
                    })
                    .collect()
            }
            Plan::Fields(entries) => entries
                .into_iter()
                .map(|(k, f)| {
                    self.push(Node {
                        kind: Kind::Field(f),
                        name: Some(k),
                        parent: Some(node),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Plan::Items(items) => items
                .into_iter()
                .map(|f| {
                    self.push(Node {
                        kind: Kind::Field(f),
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

    fn event_at(&self, node: NodeId) -> Option<(usize, usize)> {
        match self.nodes.borrow()[node.0 as usize].kind {
            Kind::Event { group, idx } => Some((group, idx)),
            _ => None,
        }
    }
}

impl AstAdapter for CwlAdapter {
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
            Kind::Group(_) => vec!["group".into()],
            Kind::Event { .. } => vec!["entry".into()],
            _ => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if let Some((g, i)) = self.event_at(node) {
            let groups = self.groups.borrow();
            let e = &groups[g].1[i];
            let envelope = match name {
                "timestamp" => Some(Value::Instant {
                    secs: e.secs,
                    nanos: e.nanos,
                    offset_min: None,
                }),
                "logStream" => e.stream.clone().map(Value::Str),
                _ => None,
            };
            if envelope.is_some() {
                return envelope;
            }
            if let Some(Field::Map(fields)) = &e.payload {
                return fields
                    .iter()
                    .find(|(k, _)| k == name)
                    .and_then(|(_, f)| f.scalar());
            }
            return None;
        }
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field(Field::Map(entries)) => entries
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar()),
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        if let Some((g, i)) = self.event_at(node) {
            let groups = self.groups.borrow();
            let e = &groups[g].1[i];
            if let Some(t) = &e.text {
                return Some(Value::Str(t.clone()));
            }
            if let Some(Field::Map(fields)) = &e.payload {
                for k in ["message", "logMessage"] {
                    if let Some(v) = fields.iter().find(|(key, _)| key == k).and_then(|(_, f)| f.scalar())
                    {
                        return Some(v);
                    }
                }
            }
            return None;
        }
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field(f) => f.scalar(),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let (g, i) = self.event_at(node)?;
        let groups = self.groups.borrow();
        let (full, events) = &groups[g];
        let e = &events[i];
        match key {
            "group" => Some(Value::Str(full.clone())),
            "stream" => e.stream.clone().map(Value::Str),
            "received" => e.received.map(|(secs, nanos)| Value::Instant {
                secs,
                nanos,
                offset_min: None,
            }),
            "id" => e.id.clone().map(Value::Str),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_mount_is_enforced() {
        assert!(matches!(parse_target("cwl:/aws/lambda/fn"), Err(CwlError::Target(_))));
        assert!(parse_target("cwl:/aws/lambda/fn?since=1h").is_ok());
        assert!(parse_target("cwl:?limit=100").is_ok());
        assert!(parse_target("cloudwatch:?since=2026-07-25T00:00:00Z").is_ok());
        assert!(matches!(
            parse_target("cwl:?since=fortnight"),
            Err(CwlError::Target(_))
        ));
        let (t, region, endpoint) =
            parse_target("cwl:api?since=1h&region=eu-west-1&endpoint=http://127.0.0.1:9/").unwrap();
        assert_eq!(t.group.as_deref(), Some("api"));
        assert_eq!(region.as_deref(), Some("eu-west-1"));
        assert_eq!(endpoint.as_deref(), Some("http://127.0.0.1:9/"));
    }

    #[test]
    fn event_decoding() {
        let e = decode_event(&serde_json::json!({
            "timestamp": 1753460402310i64,
            "message": "{\"level\": \"error\", \"message\": \"boom\", \"requestId\": \"r-9\"}",
            "logStreamName": "2026/07/25/[$LATEST]abc",
            "ingestionTime": 1753460403000i64,
            "eventId": "e-1"
        }))
        .unwrap();
        assert_eq!(e.secs, 1753460402);
        assert!(e.text.is_none());
        assert!(matches!(&e.payload, Some(Field::Map(m)) if m.len() == 3));

        let t = decode_event(&serde_json::json!({
            "timestamp": 1753460402310i64,
            "message": "plain line\n"
        }))
        .unwrap();
        assert_eq!(t.text.as_deref(), Some("plain line"));
        assert!(t.payload.is_none());
    }

    #[test]
    fn short_group_names() {
        assert_eq!(short_group("/aws/lambda/checkout"), "checkout");
        assert_eq!(short_group("my-flat-group"), "my-flat-group");
        assert_eq!(short_group("/ecs/api/"), "api");
    }
}
