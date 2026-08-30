//! Datadog Logs adapter for the Quarb query engine.
//!
//! The logs-family design sheet, over Datadog's Logs Search API:
//! a mount is a *bounded snapshot* — `since=` and/or `limit=` are
//! mandatory — of whatever a `query=` (Datadog's own search
//! syntax, applied server-side) selects across the org's indexed
//! logs. Entries sit at the root as `/entry` children, oldest
//! first.
//!
//! Datadog's envelope calls severity `status` and threads
//! correlation through `dd.trace_id`; both get family spellings
//! on top of their native ones:
//!
//! - **Properties**: `::status` (native) with the lowercased
//!   status as a trait (`/entry<error>`) and the numeric family
//!   rank at `;;;severity`; `::timestamp` (a typed instant),
//!   `::service`, `::host`, `::trace` (minted from
//!   `dd.trace_id` when present — the join key). Anything else
//!   falls through to the event's custom attributes, then its
//!   tags — a `env:prod` tag answers `::env`.
//! - **The default value** (`::`) is the log message.
//! - **Children**: the custom-attribute tree, and `tags`.
//! - **Metadata**: `;;;severity`, `;;;id`, `;;;index`.
//!
//! **Transport and auth**: `POST /api/v2/logs/events/search`
//! with `DD-API-KEY` + `DD-APPLICATION-KEY` from `$DD_API_KEY` /
//! `$DD_APP_KEY`; the site from `site=` or `$DD_SITE`
//! (`datadoghq.com` default — EU orgs want `datadoghq.eu`).
//! `endpoint=` overrides the URL (the bottled test server).
//!
//! **Target**:
//! `ddl:?since=1h&until=…&query=…&limit=N&site=…&endpoint=…`
//! (`datadog:` works too).

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use serde_json::json;
use std::cell::RefCell;

/// An error connecting to or reading Datadog Logs.
#[derive(Debug, thiserror::Error)]
pub enum DdlError {
    #[error("datadog: {0}")]
    Api(String),
    #[error(
        "ddlogs target: {0} (expected ddl:?since=1h&query=…&limit=N; \
         a mount must be bounded — give it since= and/or limit=)"
    )]
    Target(String),
}

/// One decoded attribute node: a JSON tree or a scalar.
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
    offset_min: Option<i16>,
    status: String,
    rank: i64,
    service: Option<String>,
    host: Option<String>,
    message: Option<String>,
    /// `dd.trace_id`, when present.
    trace: Option<String>,
    id: Option<String>,
    index: Option<String>,
    /// The event's custom attributes.
    attrs: Vec<(String, Field)>,
    /// `key:value` tags, split.
    tags: Vec<(String, String)>,
}

enum Kind {
    Root,
    Entry(usize),
    Field(Field),
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A bounded snapshot of Datadog log events.
pub struct DdlAdapter {
    events: Vec<Event>,
    nodes: RefCell<Vec<Node>>,
}

/// The family severity rank for Datadog's status words.
fn status_rank(s: &str) -> i64 {
    match s.to_ascii_lowercase().as_str() {
        "emergency" => 800,
        "alert" => 700,
        "critical" => 600,
        "error" => 500,
        "warn" | "warning" => 400,
        "notice" => 300,
        "info" => 200,
        "debug" | "trace" => 100,
        _ => 0,
    }
}

/// A duration suffix in seconds (`30m` → 1800), for resolving a
/// relative window against the pinned invocation instant.
fn duration_secs(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    })
}

fn is_duration(s: &str) -> bool {
    s.len() >= 2
        && s.ends_with(['s', 'm', 'h', 'd'])
        && s[..s.len() - 1].chars().all(|c| c.is_ascii_digit())
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

struct Target {
    since: Option<String>,
    until: Option<String>,
    query: Option<String>,
    limit: Option<u64>,
    site: Option<String>,
    endpoint: Option<String>,
}

fn parse_target(target: &str) -> Result<Target, DdlError> {
    let rest = target
        .strip_prefix("ddl:")
        .or_else(|| target.strip_prefix("datadog:"))
        .ok_or_else(|| DdlError::Target(target.to_string()))?;
    let rest = rest.trim_start_matches("//").trim_start_matches('?');
    let query_string = rest;
    let param = |k: &str| {
        query_string
            .split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(percent_decode))
    };
    let since = param("since");
    if let Some(s) = &since
        && !is_duration(s)
        && parse_iso(s).is_none()
    {
        return Err(DdlError::Target(format!(
            "since={s}: not a duration (30m, 1h, 2d) or an ISO instant"
        )));
    }
    let until = param("until");
    if let Some(u) = &until
        && parse_iso(u).is_none()
    {
        return Err(DdlError::Target(format!("until={u}: not an ISO instant")));
    }
    let limit = match param("limit") {
        None => None,
        Some(l) => Some(
            l.parse::<u64>()
                .map_err(|_| DdlError::Target(format!("limit={l}: not a number")))?,
        ),
    };
    if since.is_none() && limit.is_none() {
        return Err(DdlError::Target(format!(
            "{target}: unbounded — an org's indexes hold more logs than \
             you want to snapshot"
        )));
    }
    Ok(Target {
        since,
        until,
        query: param("query"),
        limit,
        site: param("site"),
        endpoint: param("endpoint"),
    })
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

/// Decode one v2 log event.
fn decode_event(o: &Json) -> Option<Event> {
    let a = o.pointer("/attributes")?;
    let ts = a.pointer("/timestamp")?.as_str()?;
    let (secs, nanos, offset_min) = parse_iso(ts)?;
    let status = a
        .pointer("/status")
        .and_then(|v| v.as_str())
        .unwrap_or("info")
        .to_string();
    let attrs: Vec<(String, Field)> = a
        .pointer("/attributes")
        .and_then(|v| v.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), decode_json(v))).collect())
        .unwrap_or_default();
    let trace = a
        .pointer("/attributes/dd/trace_id")
        .and_then(|v| match v {
            Json::String(s) => Some(s.clone()),
            Json::Number(n) => Some(n.to_string()),
            _ => None,
        });
    let tags: Vec<(String, String)> = a
        .pointer("/tags")
        .and_then(|v| v.as_array())
        .map(|ts| {
            ts.iter()
                .filter_map(|t| t.as_str())
                .filter_map(|t| t.split_once(':').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(Event {
        secs,
        nanos,
        offset_min,
        rank: status_rank(&status),
        status,
        service: a
            .pointer("/service")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        host: a
            .pointer("/host")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        message: a
            .pointer("/message")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        trace,
        id: o.pointer("/id").and_then(|v| v.as_str()).map(str::to_string),
        index: a
            .pointer("/index")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        attrs,
        tags,
    })
}

impl DdlAdapter {
    /// Open a bounded snapshot of the org's logs.
    pub fn open(target: &str) -> Result<Self, DdlError> {
        let t = parse_target(target)?;
        let api_key = std::env::var("DD_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| DdlError::Api("no $DD_API_KEY in the environment".into()))?;
        let app_key = std::env::var("DD_APP_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| DdlError::Api("no $DD_APP_KEY in the environment".into()))?;
        let site = t
            .site
            .clone()
            .or_else(|| std::env::var("DD_SITE").ok().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "datadoghq.com".into());
        let base = t
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://api.{site}"));
        let url = format!("{}/api/v2/logs/events/search", base.trim_end_matches('/'));

        // With the invocation instant pinned (`qua --now`), a
        // relative window resolves against *it* rather than the
        // provider's clock: the request carries absolute bounds,
        // so a pinned run replays. Unpinned, the provider's own
        // `now-…` arithmetic stands as before.
        let pinned = quarb::invocation_instant().map(|(secs, _)| secs);
        let (from, to) = match pinned {
            Some(now) => {
                let iso = |secs: i64| quarb::temporal::format_instant(secs, 0, Some(0));
                let from = match &t.since {
                    Some(s) if is_duration(s) => iso(now - duration_secs(s).unwrap_or(0)),
                    Some(s) => s.clone(),
                    None => iso(now - 900),
                };
                let to = t.until.clone().unwrap_or_else(|| iso(now));
                (from, to)
            }
            None => {
                let from = match &t.since {
                    Some(s) if is_duration(s) => format!("now-{s}"),
                    Some(s) => s.clone(),
                    None => "now-15m".into(),
                };
                (from, t.until.clone().unwrap_or_else(|| "now".into()))
            }
        };

        let mut events: Vec<Event> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut filter = json!({ "from": from, "to": to });
            if let Some(q) = &t.query
                && !q.trim().is_empty()
            {
                filter["query"] = json!(q.trim());
            }
            let page_limit = t
                .limit
                .map(|l| (l - (events.len() as u64).min(l)).clamp(1, 1000))
                .unwrap_or(1000);
            let mut page = json!({ "limit": page_limit });
            if let Some(c) = &cursor {
                page["cursor"] = json!(c);
            }
            let body = json!({ "filter": filter, "page": page, "sort": "timestamp" });
            let resp = ureq::post(&url)
                .set("DD-API-KEY", api_key.trim())
                .set("DD-APPLICATION-KEY", app_key.trim())
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
                .map_err(|e| DdlError::Api(format!("logs/events/search: {e}")))?;
            let text = resp
                .into_string()
                .map_err(|e| DdlError::Api(format!("logs/events/search: {e}")))?;
            let doc: Json = serde_json::from_str(&text)
                .map_err(|e| DdlError::Api(format!("logs/events/search: {e}")))?;
            for e in doc
                .pointer("/data")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(ev) = decode_event(e) {
                    events.push(ev);
                }
            }
            if let Some(l) = t.limit
                && events.len() as u64 >= l
            {
                events.truncate(l as usize);
                break;
            }
            cursor = doc
                .pointer("/meta/page/after")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(Self::from_events(events))
    }

    /// Build from already-fetched v2 log-event objects — the test
    /// fixture's entry point.
    pub fn from_json(docs: &[Json]) -> Self {
        Self::from_events(docs.iter().filter_map(decode_event).collect())
    }

    fn from_events(mut events: Vec<Event>) -> Self {
        events.sort_by(|a, b| (a.secs, a.nanos, &a.id).cmp(&(b.secs, b.nanos, &b.id)));
        let adapter = DdlAdapter {
            events,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        let ids: Vec<NodeId> = (0..adapter.events.len())
            .map(|i| {
                adapter.push(Node {
                    kind: Kind::Entry(i),
                    name: Some("entry".into()),
                    parent: Some(NodeId(0)),
                    children: RefCell::new(None),
                })
            })
            .collect();
        *adapter.nodes.borrow()[0].children.borrow_mut() = Some(ids);
        adapter
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// A human-readable locator: `/entry[N]/…`.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let nodes = self.nodes.borrow();
            let n = &nodes[id.0 as usize];
            match &n.kind {
                Kind::Root => {}
                Kind::Entry(i) => parts.push(format!("entry[{}]", i + 1)),
                Kind::Field(_) => parts.push(n.name.clone().unwrap_or_default()),
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
            Entry(usize),
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
                    Kind::Entry(i) => Plan::Entry(*i),
                    Kind::Field(Field::Map(entries)) => Plan::Fields(entries.clone()),
                    Kind::Field(Field::List(items)) => Plan::Items(items.clone()),
                    Kind::Field(Field::Scalar(_)) => Plan::Leaf,
                }
            }
        };
        let made = match plan {
            Plan::Done(k) => return k,
            Plan::Leaf => Vec::new(),
            Plan::Entry(i) => {
                let e = &self.events[i];
                let mut kids: Vec<(String, Field)> = e.attrs.clone();
                if !e.tags.is_empty() {
                    kids.push((
                        "tags".into(),
                        Field::Map(
                            e.tags
                                .iter()
                                .map(|(k, v)| (k.clone(), Field::Scalar(str_value(v))))
                                .collect(),
                        ),
                    ));
                }
                kids.into_iter()
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

    fn entry_of(&self, node: NodeId) -> Option<usize> {
        match self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entry(i) => Some(i),
            _ => None,
        }
    }
}

impl AstAdapter for DdlAdapter {
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
        match self.entry_of(node) {
            Some(i) => vec![
                "entry".into(),
                self.events[i].status.to_ascii_lowercase(),
            ],
            None => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if let Some(i) = self.entry_of(node) {
            let e = &self.events[i];
            let envelope = match name {
                "status" => Some(Value::Str(e.status.clone())),
                "timestamp" => Some(Value::Instant {
                    secs: e.secs,
                    nanos: e.nanos,
                    offset_min: e.offset_min,
                }),
                "service" => e.service.clone().map(Value::Str),
                "host" => e.host.clone().map(Value::Str),
                "trace" => e.trace.clone().map(Value::Str),
                _ => None,
            };
            if envelope.is_some() {
                return envelope;
            }
            if let Some(v) = e
                .attrs
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar())
            {
                return Some(v);
            }
            if let Some((_, v)) = e.tags.iter().find(|(k, _)| k == name) {
                return Some(str_value(v));
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
        if let Some(i) = self.entry_of(node) {
            return self.events[i].message.clone().map(Value::Str);
        }
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field(f) => f.scalar(),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let i = self.entry_of(node)?;
        let e = &self.events[i];
        match key {
            "severity" => Some(Value::Int(e.rank)),
            "id" => e.id.clone().map(Value::Str),
            "index" => e.index.clone().map(Value::Str),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<Json> {
        serde_json::from_str(
            r#"[
              {"id": "d-2", "type": "log", "attributes": {
                "timestamp": "2026-07-25T14:02:07.610Z",
                "status": "error", "service": "checkout",
                "host": "gke-node-7",
                "message": "processor timeout",
                "index": "main",
                "tags": ["env:prod", "team:payments"],
                "attributes": {"order": "o-1402", "timeout_ms": 2800,
                               "dd": {"trace_id": "77cc41090e"}}}},
              {"id": "d-1", "type": "log", "attributes": {
                "timestamp": "2026-07-25T14:02:07.550Z",
                "status": "info", "service": "gateway",
                "message": "POST /pay 500",
                "tags": ["env:prod"],
                "attributes": {"http": {"status_code": 500},
                               "dd": {"trace_id": "77cc41090e"}}}}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn bounded_mount_is_enforced() {
        assert!(matches!(parse_target("ddl:"), Err(DdlError::Target(_))));
        assert!(matches!(
            parse_target("ddl:?query=service:checkout"),
            Err(DdlError::Target(_))
        ));
        assert!(parse_target("ddl:?since=1h").is_ok());
        assert!(parse_target("datadog:?limit=500&site=datadoghq.eu").is_ok());
    }

    #[test]
    fn events_decode_and_query() {
        let a = DdlAdapter::from_json(&fixture());
        let run = |q: &str| match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
        };
        // Oldest first; :: is the message; the status is a trait
        // and an ordered rank.
        assert_eq!(run("/entry::"), vec!["POST /pay 500", "processor timeout"]);
        assert_eq!(run("/entry<error>::service"), vec!["checkout"]);
        assert_eq!(run("/entry[::::severity >= 400] @| count"), vec!["1"]);
        // Fallthrough: custom attributes, then tags.
        assert_eq!(run("/entry[::order]::order"), vec!["o-1402"]);
        assert_eq!(run("/entry[::team]::team"), vec!["payments"]);
        // Nested custom attributes navigate as children.
        assert_eq!(run("/entry[/http/status_code:: = 500]::service"), vec!["gateway"]);
        // The minted trace joins the two services.
        let joined = run(
            "/entry<error> <=> /entry[::service = 'gateway'][::trace = _::trace] \
             | %(::order; edge = $$1::)",
        );
        assert_eq!(joined, vec!["%(order = \"o-1402\"; edge = \"POST /pay 500\")"]);
    }
}
