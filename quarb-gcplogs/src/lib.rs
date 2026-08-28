//! Google Cloud Logging adapter for the Quarb query engine.
//!
//! A mount is a *bounded snapshot* of a project's log entries —
//! never a tail. The bound is explicit (the kafka doctrine: cost
//! is stated, not discovered): the target must carry `since=`
//! and/or `limit=`, and a bare `gcl:project` is refused with the
//! spelling of the fix.
//!
//! Entries sit at the root as `/entry` children (the `/row`
//! convention), oldest first, whatever order the API returned
//! them in. The LogEntry envelope is uniform and its payloads are
//! not — which is exactly the heterogeneous-tree shape the
//! language is for: `[/jsonPayload/latency]` discriminates by
//! shape, `::severity` filters by level, and two entries that
//! share a `::trace` correlate with an ordinary `<=>` join —
//! reconstructing a request across services in one query.
//!
//! - **Properties**: `::severity`, `::timestamp` (a typed
//!   instant), `::logName` (the short log id, URL-decoded),
//!   `::trace` (the bare trace id — the join key), `::spanId`,
//!   `::insertId`; anything else falls through to the
//!   `jsonPayload` top level, then `labels`, then
//!   `resource.labels`, then `httpRequest` — labels are how
//!   logs name things, so `::service` just works.
//! - **The default value** (`::`) is `textPayload`, or the
//!   `jsonPayload.message` convention — `/entry::` reads the
//!   messages.
//! - **Traits**: every entry carries `entry` and its lowercased
//!   severity, so `/entry<error>` selects a level structurally.
//! - **Metadata**: `;;;severity` (the numeric rank, ordered —
//!   `[;;;severity >= 400]` is WARNING and up), `;;;received`,
//!   `;;;log` (the full logName), `;;;project`.
//!
//! **Transport and auth**: the adapter shells out to
//! `gcloud logging read --format=json` (kubectl-adapter posture:
//! plumbing over subprocess, zero new dependencies), so ADC,
//! configurations, and impersonation behave exactly as gcloud
//! does. `QUARB_GCLOUD` overrides the binary — the test fixture
//! and wrapper hook.
//!
//! **Target**:
//! `gcl:PROJECT?since=1h&until=…&filter=…&limit=N&order=…`
//! (`gcplogs:` works too). `since` takes a duration (`30m`,
//! `1h`, `2d` — gcloud freshness) or an ISO instant; `until`
//! an ISO instant; `filter` is a raw Cloud Logging filter
//! expression, ANDed with the time bounds — the place to push
//! `severity>=WARNING` or `resource.type="cloud_run_revision"`
//! so the cut happens server-side. `account=` picks the gcloud
//! account for multi-account setups (the BigQuery pattern).

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;

/// An error connecting to or reading Cloud Logging.
#[derive(Debug, thiserror::Error)]
pub enum GclError {
    #[error("gcloud: {0}")]
    Gcloud(String),
    #[error(
        "gcplogs target: {0} (expected gcl:PROJECT?since=1h&filter=…&limit=N; \
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

/// One log entry, decoded from the API's JSON.
#[derive(Clone)]
struct Entry {
    severity: String,
    /// The numeric severity rank (GCP's own enum values).
    rank: i64,
    secs: i64,
    nanos: u32,
    offset_min: Option<i16>,
    received: Option<(i64, u32, Option<i16>)>,
    /// The full logName resource path.
    log_full: String,
    /// The short log id (`logName` minus the project prefix,
    /// URL-decoded).
    log: String,
    /// The bare trace id (`trace` minus the project prefix).
    trace: Option<String>,
    span_id: Option<String>,
    insert_id: Option<String>,
    text: Option<String>,
    /// `jsonPayload` (or `protoPayload`) as a field tree.
    payload: Option<Field>,
    labels: Vec<(String, String)>,
    resource_type: Option<String>,
    resource_labels: Vec<(String, String)>,
    http: Vec<(String, Field)>,
}

enum Kind {
    Root,
    Entry(usize),
    /// A named subtree of an entry (`resource`, `labels`,
    /// `httpRequest`) or a payload field.
    Field(Field),
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A bounded snapshot of a project's log entries.
pub struct GclAdapter {
    project: String,
    entries: Vec<Entry>,
    nodes: RefCell<Vec<Node>>,
}

/// GCP's severity enum, by rank.
fn severity_rank(s: &str) -> i64 {
    match s {
        "DEBUG" => 100,
        "INFO" => 200,
        "NOTICE" => 300,
        "WARNING" => 400,
        "ERROR" => 500,
        "CRITICAL" => 600,
        "ALERT" => 700,
        "EMERGENCY" => 800,
        _ => 0, // DEFAULT
    }
}

/// A freshness duration in seconds, for resolving a relative
/// window against the pinned invocation instant.
fn freshness_secs(s: &str) -> Option<i64> {
    if !is_duration(s) {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    })
}

/// A gcloud freshness duration: `30m`, `1h`, `2d`.
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

/// The parsed mount target.
struct Target {
    project: String,
    since: Option<String>,
    until: Option<String>,
    filter: Option<String>,
    limit: Option<u64>,
    ascending: bool,
    /// `account=` — the gcloud account to authenticate as (the
    /// BigQuery adapter's pattern), for multi-account setups.
    account: Option<String>,
}

fn parse_target(target: &str) -> Result<Target, GclError> {
    let rest = target
        .strip_prefix("gcl:")
        .or_else(|| target.strip_prefix("gcplogs:"))
        .ok_or_else(|| GclError::Target(target.to_string()))?;
    let rest = rest.trim_start_matches("//");
    let (project, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    if project.is_empty() {
        return Err(GclError::Target(target.to_string()));
    }
    let param = |k: &str| {
        query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(percent_decode))
        })
    };
    let since = param("since");
    if let Some(s) = &since
        && !is_duration(s)
        && parse_iso(s).is_none()
    {
        return Err(GclError::Target(format!(
            "since={s}: not a duration (30m, 1h, 2d) or an ISO instant"
        )));
    }
    let until = param("until");
    if let Some(u) = &until
        && parse_iso(u).is_none()
    {
        return Err(GclError::Target(format!("until={u}: not an ISO instant")));
    }
    let limit = match param("limit") {
        None => None,
        Some(l) => Some(l.parse::<u64>().map_err(|_| {
            GclError::Target(format!("limit={l}: not a number"))
        })?),
    };
    // The explicit-cost gate: an unbounded mount is refused, with
    // the fix in the message.
    if since.is_none() && limit.is_none() {
        return Err(GclError::Target(format!(
            "{target}: unbounded — a busy project holds more logs than \
             you want to snapshot"
        )));
    }
    Ok(Target {
        project: project.to_string(),
        since,
        until,
        filter: param("filter"),
        limit,
        ascending: param("order").as_deref() != Some("desc"),
        account: param("account"),
    })
}

/// Compose the Cloud Logging filter expression: the user's filter
/// ANDed with any ISO time bounds (durations ride `--freshness`).
fn compose_filter(t: &Target) -> String {
    let mut clauses = Vec::new();
    if let Some(f) = &t.filter
        && !f.trim().is_empty()
    {
        clauses.push(format!("({})", f.trim()));
    }
    if let Some(s) = &t.since {
        // Pinned invocation instant: a duration window becomes an
        // absolute bound in the filter (and `--freshness` is
        // dropped at the call site), so a pinned run replays.
        match (quarb::invocation_instant(), freshness_secs(s)) {
            (Some((now, _)), Some(d)) => {
                let at = quarb::temporal::format_instant(now - d, 0, Some(0));
                clauses.push(format!("timestamp>=\"{at}\""));
            }
            _ if !is_duration(s) => clauses.push(format!("timestamp>=\"{s}\"")),
            _ => {}
        }
    }
    if let Some(u) = &t.until {
        clauses.push(format!("timestamp<=\"{u}\""));
    }
    clauses.join(" AND ")
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

fn str_pairs(v: Option<&Json>) -> Vec<(String, String)> {
    v.and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Decode one LogEntry object.
fn decode_entry(o: &Json, project: &str) -> Option<Entry> {
    let ts = o.pointer("/timestamp")?.as_str()?;
    let (secs, nanos, offset_min) = parse_iso(ts)?;
    let severity = o
        .pointer("/severity")
        .and_then(|v| v.as_str())
        .unwrap_or("DEFAULT")
        .to_string();
    let log_full = o
        .pointer("/logName")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let log = percent_decode(
        log_full
            .strip_prefix(&format!("projects/{project}/logs/"))
            .unwrap_or(&log_full),
    );
    let trace = o.pointer("/trace").and_then(|v| v.as_str()).map(|t| {
        t.strip_prefix(&format!("projects/{project}/traces/"))
            .unwrap_or(t)
            .to_string()
    });
    let payload = o
        .pointer("/jsonPayload")
        .or_else(|| o.pointer("/protoPayload"))
        .map(decode_json);
    Some(Entry {
        rank: severity_rank(&severity),
        severity,
        secs,
        nanos,
        offset_min,
        received: o
            .pointer("/receiveTimestamp")
            .and_then(|v| v.as_str())
            .and_then(parse_iso),
        log_full,
        log,
        trace,
        span_id: o
            .pointer("/spanId")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        insert_id: o
            .pointer("/insertId")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        text: o
            .pointer("/textPayload")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        payload,
        labels: str_pairs(o.pointer("/labels")),
        resource_type: o
            .pointer("/resource/type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        resource_labels: str_pairs(o.pointer("/resource/labels")),
        http: o
            .pointer("/httpRequest")
            .and_then(|v| v.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), decode_json(v))).collect())
            .unwrap_or_default(),
    })
}

impl GclAdapter {
    /// Open a bounded snapshot of `target`'s log entries via
    /// `gcloud logging read`.
    pub fn open(target: &str) -> Result<Self, GclError> {
        let t = parse_target(target)?;
        let bin = std::env::var("QUARB_GCLOUD").unwrap_or_else(|_| "gcloud".into());
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("logging")
            .arg("read")
            .arg(compose_filter(&t))
            .arg("--project")
            .arg(&t.project)
            .arg("--format=json");
        // Unpinned, the duration rides `--freshness`; pinned, the
        // absolute bound is already in the filter.
        if let Some(s) = &t.since
            && is_duration(s)
            && quarb::invocation_instant().is_none()
        {
            cmd.arg(format!("--freshness={s}"));
        }
        if let Some(l) = t.limit {
            cmd.arg(format!("--limit={l}"));
        }
        if let Some(a) = &t.account {
            cmd.arg(format!("--account={a}"));
        }
        let out = cmd
            .output()
            .map_err(|e| GclError::Gcloud(format!("running {bin}: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(GclError::Gcloud(err.trim().to_string()));
        }
        let docs: Vec<Json> = serde_json::from_slice(&out.stdout)
            .map_err(|e| GclError::Gcloud(format!("parsing gcloud output: {e}")))?;
        Ok(Self::from_json(&t.project, &docs, t.ascending))
    }

    /// Build the arbor from already-fetched LogEntry objects — the
    /// deterministic core `open` wraps, and the test fixture's
    /// entry point.
    pub fn from_json(project: &str, docs: &[Json], ascending: bool) -> Self {
        let mut entries: Vec<Entry> = docs.iter().filter_map(|o| decode_entry(o, project)).collect();
        // Chronological reading order regardless of fetch order;
        // insertId breaks timestamp ties deterministically.
        entries.sort_by(|a, b| {
            (a.secs, a.nanos, &a.insert_id).cmp(&(b.secs, b.nanos, &b.insert_id))
        });
        if !ascending {
            entries.reverse();
        }
        let adapter = GclAdapter {
            project: project.to_string(),
            entries,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        let ids: Vec<NodeId> = (0..adapter.entries.len())
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

    /// A human-readable locator: `/entry[N]/…` by snapshot
    /// position, then field names.
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

    /// Materialize a node's children lazily (payload trees can be
    /// wide; most queries never descend into most entries).
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
                    Kind::Root => Plan::Leaf, // filled at build
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
                let e = &self.entries[i];
                let mut kids: Vec<(String, Field)> = Vec::new();
                if let Some(Field::Map(fields)) = &e.payload {
                    kids.extend(fields.clone());
                }
                if e.resource_type.is_some() || !e.resource_labels.is_empty() {
                    let mut m: Vec<(String, Field)> = e
                        .resource_labels
                        .iter()
                        .map(|(k, v)| (k.clone(), Field::Scalar(str_value(v))))
                        .collect();
                    if let Some(t) = &e.resource_type {
                        m.insert(0, ("type".into(), Field::Scalar(Value::Str(t.clone()))));
                    }
                    kids.push(("resource".into(), Field::Map(m)));
                }
                if !e.labels.is_empty() {
                    kids.push((
                        "labels".into(),
                        Field::Map(
                            e.labels
                                .iter()
                                .map(|(k, v)| (k.clone(), Field::Scalar(str_value(v))))
                                .collect(),
                        ),
                    ));
                }
                if !e.http.is_empty() {
                    kids.push(("httpRequest".into(), Field::Map(e.http.clone())));
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

impl AstAdapter for GclAdapter {
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
                self.entries[i].severity.to_ascii_lowercase(),
            ],
            None => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if let Some(i) = self.entry_of(node) {
            let e = &self.entries[i];
            let envelope = match name {
                "severity" => Some(Value::Str(e.severity.clone())),
                "timestamp" => Some(Value::Instant {
                    secs: e.secs,
                    nanos: e.nanos,
                    offset_min: e.offset_min,
                }),
                "logName" => Some(Value::Str(e.log.clone())),
                "trace" => e.trace.clone().map(Value::Str),
                "spanId" => e.span_id.clone().map(Value::Str),
                "insertId" => e.insert_id.clone().map(Value::Str),
                _ => None,
            };
            if envelope.is_some() {
                return envelope;
            }
            // Fallthrough: payload top level, then labels, then
            // resource labels, then the http request — labels are
            // how logs name things.
            if let Some(Field::Map(fields)) = &e.payload
                && let Some(v) = fields.iter().find(|(k, _)| k == name).and_then(|(_, f)| f.scalar())
            {
                return Some(v);
            }
            if let Some((_, v)) = e.labels.iter().find(|(k, _)| k == name) {
                return Some(str_value(v));
            }
            if let Some((_, v)) = e.resource_labels.iter().find(|(k, _)| k == name) {
                return Some(str_value(v));
            }
            return e
                .http
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar());
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
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entry(i) => {
                let e = &self.entries[*i];
                if let Some(t) = &e.text {
                    return Some(Value::Str(t.clone()));
                }
                // The structured-logging convention: a `message`
                // (or `logMessage`) field carries the human line.
                if let Some(Field::Map(fields)) = &e.payload {
                    let get = |k: &str| {
                        fields
                            .iter()
                            .find(|(key, _)| key == k)
                            .and_then(|(_, f)| f.scalar())
                    };
                    for k in ["message", "logMessage"] {
                        if let Some(v) = get(k) {
                            return Some(v);
                        }
                    }
                    // A request log (App Engine's RequestLog shape):
                    // synthesize the summary line the Logs Explorer
                    // shows — METHOD resource STATUS.
                    if let (Some(m), Some(r)) = (get("method"), get("resource")) {
                        let status = get("status")
                            .map(|s| format!(" {s}"))
                            .unwrap_or_default();
                        return Some(Value::Str(format!("{m} {r}{status}")));
                    }
                }
                // A bare request entry (Cloud Run's requests log):
                // the same summary line from the HTTP envelope.
                if !e.http.is_empty() {
                    let get = |k: &str| {
                        e.http.iter().find(|(key, _)| key == k).and_then(|(_, f)| f.scalar())
                    };
                    if let (Some(m), Some(u)) = (get("requestMethod"), get("requestUrl")) {
                        let status = get("status")
                            .map(|s| format!(" {s}"))
                            .unwrap_or_default();
                        return Some(Value::Str(format!("{m} {u}{status}")));
                    }
                }
                None
            }
            Kind::Field(f) => f.scalar(),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let i = self.entry_of(node)?;
        let e = &self.entries[i];
        match key {
            "severity" => Some(Value::Int(e.rank)),
            "received" => e.received.map(|(secs, nanos, offset_min)| Value::Instant {
                secs,
                nanos,
                offset_min,
            }),
            "log" => Some(Value::Str(e.log_full.clone())),
            "project" => Some(Value::Str(self.project.clone())),
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
              {"timestamp": "2026-07-25T10:00:02Z", "severity": "ERROR",
               "logName": "projects/demo/logs/run.googleapis.com%2Fstderr",
               "trace": "projects/demo/traces/abc123",
               "insertId": "e2",
               "jsonPayload": {"message": "payment failed", "latency_ms": 1200,
                               "order": "o-77"},
               "resource": {"type": "cloud_run_revision",
                            "labels": {"service_name": "checkout"}}},
              {"timestamp": "2026-07-25T10:00:01Z", "severity": "INFO",
               "logName": "projects/demo/logs/run.googleapis.com%2Fstdout",
               "trace": "projects/demo/traces/abc123",
               "insertId": "e1",
               "textPayload": "handling /pay for o-77",
               "httpRequest": {"requestMethod": "POST", "status": 200,
                               "requestUrl": "https://shop.example/pay"},
               "labels": {"service": "gateway"}},
              {"timestamp": "2026-07-24T23:59:00Z", "severity": "WARNING",
               "logName": "projects/demo/logs/app",
               "insertId": "e0",
               "jsonPayload": {"message": "cache miss", "key": "user:9"}}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn bounded_mount_is_enforced() {
        assert!(matches!(parse_target("gcl:demo"), Err(GclError::Target(_))));
        assert!(parse_target("gcl:demo?since=1h").is_ok());
        assert!(parse_target("gcl:demo?limit=100").is_ok());
        assert!(parse_target("gcplogs:demo?since=2026-07-25T00:00:00Z").is_ok());
        assert!(matches!(
            parse_target("gcl:demo?since=yesterday"),
            Err(GclError::Target(_))
        ));
        assert!(matches!(parse_target("gcl:?since=1h"), Err(GclError::Target(_))));
    }

    #[test]
    fn filter_composition() {
        let t = parse_target(
            "gcl:demo?filter=severity>=WARNING&since=2026-07-01T00:00:00Z&until=2026-07-02T00:00:00Z",
        )
        .unwrap();
        assert_eq!(
            compose_filter(&t),
            "(severity>=WARNING) AND timestamp>=\"2026-07-01T00:00:00Z\" \
             AND timestamp<=\"2026-07-02T00:00:00Z\""
        );
        // A duration since rides --freshness, not the filter.
        let t = parse_target("gcl:demo?since=1h").unwrap();
        assert_eq!(compose_filter(&t), "");
    }

    #[test]
    fn arbor_shape_and_values() {
        let a = GclAdapter::from_json("demo", &fixture(), true);
        let run = |q: &str| match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
        };
        // Chronological order, oldest first; :: reads the message
        // whichever payload shape carries it.
        assert_eq!(
            run("/entry::"),
            vec!["cache miss", "handling /pay for o-77", "payment failed"]
        );
        // Severity as text, trait, and ordered metadata rank.
        assert_eq!(run("/entry[::severity = 'ERROR']::insertId"), vec!["e2"]);
        assert_eq!(run("/entry<error>::insertId"), vec!["e2"]);
        assert_eq!(run("/entry[;;;severity >= 400] @| count"), vec!["2"]);
        // The instant is typed: calendar comparison works.
        assert_eq!(run("/entry[::timestamp > 2026-07-25] @| count"), vec!["2"]);
        // Short log ids, URL-decoded.
        assert_eq!(run("/entry[1]::logName"), vec!["app"]);
        assert_eq!(run("/entry[-1]::logName"), vec!["run.googleapis.com/stderr"]);
        // Fallthrough: payload fields, labels, resource labels,
        // http request.
        assert_eq!(run("/entry[::key]::key"), vec!["user:9"]);
        assert_eq!(run("/entry[::service]::service"), vec!["gateway"]);
        assert_eq!(run("/entry[::service_name]::service_name"), vec!["checkout"]);
        assert_eq!(run("/entry[::status = 200]::requestMethod"), vec!["POST"]);
        // Payload subtree navigation and shape discrimination.
        assert_eq!(run("/entry[/latency_ms:: > 1000]::order"), vec!["o-77"]);
        assert_eq!(run("/entry[/httpRequest] @| count"), vec!["1"]);
    }

    /// Request logs carry no message; `::` synthesizes the Logs
    /// Explorer's own summary line — from the protoPayload shape
    /// (App Engine) or the HTTP envelope (Cloud Run).
    #[test]
    fn request_logs_synthesize_a_summary_line() {
        let docs: Vec<Json> = serde_json::from_str(
            r#"[
              {"timestamp": "2026-07-25T10:00:00Z", "severity": "ERROR",
               "logName": "projects/demo/logs/appengine.googleapis.com%2Frequest_log",
               "insertId": "r1",
               "protoPayload": {"method": "GET", "resource": "/t/18e4",
                                "status": 200, "latency": "4.9s"}},
              {"timestamp": "2026-07-25T10:00:01Z", "severity": "INFO",
               "logName": "projects/demo/logs/run.googleapis.com%2Frequests",
               "insertId": "r2",
               "httpRequest": {"requestMethod": "POST",
                               "requestUrl": "https://x/api", "status": 503}}
            ]"#,
        )
        .unwrap();
        let a = GclAdapter::from_json("demo", &docs, true);
        let got = match quarb::run("/entry::", &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            _ => panic!("expected values"),
        };
        assert_eq!(got, vec!["GET /t/18e4 200", "POST https://x/api 503"]);
    }

    #[test]
    fn trace_join_reconstructs_a_request() {
        let a = GclAdapter::from_json("demo", &fixture(), true);
        let got = match quarb::run(
            "/entry<error> <=> /entry<info>[::trace = $$::trace] \
             | rec(\"failed\", ::, \"upstream\", $*1::)",
            &a,
        )
        .unwrap()
        {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            _ => panic!("expected values"),
        };
        assert_eq!(
            got,
            vec!["%(failed = 'payment failed'; upstream = 'handling /pay for o-77')"]
        );
    }
}
