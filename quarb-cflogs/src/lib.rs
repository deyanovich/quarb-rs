//! Cloudflare edge-log adapter for the Quarb query engine.
//!
//! The logs family's edge member: a bounded Logpull snapshot of a
//! zone's HTTP request records. Every request Cloudflare serves
//! carries a **Ray ID**, and Cloudflare hands the same id to your
//! origin in the `cf-ray` header — which your origin logs can
//! echo. That makes the family's correlation move
//! *cross-provider*: mount the edge next to your origin's logs
//! and `<=>` on the ray, and a 500 at the edge lines up with the
//! origin lines written while serving exactly that request.
//!
//! Entries sit at the root as `/entry` children, oldest first,
//! one per request record. Logpull fields are properties under
//! their own names (`::EdgeResponseStatus`,
//! `::ClientRequestURI`, …); two conveniences are minted on top:
//! `::ray` (the RayID) and `::timestamp` (EdgeStartTimestamp as
//! a typed instant). The default value (`::`) is the request
//! line — `METHOD host+URI STATUS`.
//!
//! **Bounding**: Logpull itself demands a time range, so
//! `since=` is mandatory (`limit=` maps to Logpull's `count=`).
//! The API keeps roughly a week of logs and refuses ranges
//! newer than about a minute; the adapter clamps `until` for
//! you.
//!
//! **Transport and auth**: `GET
//! /client/v4/zones/{zone}/logs/received` returning NDJSON, with
//! a `$CLOUDFLARE_API_TOKEN` bearer token. (Logpull is an
//! Enterprise feature on real zones; `endpoint=` points the
//! adapter at Logpush archives replayed by a local server — the
//! bottled fixture — or any compatible endpoint.)
//!
//! **Target**:
//! `cfl:ZONE_ID?since=1h&until=…&limit=N&fields=…&endpoint=…`
//! (`cflogs:` works too). `fields=` overrides the default
//! field set (comma-separated Logpull field names).

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;

/// An error connecting to or reading Cloudflare logs.
#[derive(Debug, thiserror::Error)]
pub enum CflError {
    #[error("cloudflare logs: {0}")]
    Api(String),
    #[error(
        "cflogs target: {0} (expected cfl:ZONE_ID?since=1h&limit=N; \
         Logpull wants a time range — since= is mandatory)"
    )]
    Target(String),
}

const DEFAULT_FIELDS: &str = "RayID,EdgeStartTimestamp,ClientRequestMethod,\
ClientRequestHost,ClientRequestURI,EdgeResponseStatus,OriginResponseStatus,\
OriginResponseTime,ClientIP,CacheCacheStatus";

/// One request record: the chosen fields, decoded.
#[derive(Clone)]
struct Rec {
    secs: i64,
    nanos: u32,
    fields: Vec<(String, Value)>,
}

enum Kind {
    Root,
    Entry(usize),
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A bounded snapshot of a zone's edge requests.
pub struct CflAdapter {
    zone: String,
    recs: Vec<Rec>,
    nodes: RefCell<Vec<Node>>,
}

fn duration_secs(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return None,
    })
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
    zone: String,
    since_secs: i64,
    until_secs: Option<i64>,
    limit: Option<u64>,
    fields: String,
}

fn parse_target(target: &str, now_secs: i64) -> Result<(Target, Option<String>), CflError> {
    let rest = target
        .strip_prefix("cfl:")
        .or_else(|| target.strip_prefix("cflogs:"))
        .ok_or_else(|| CflError::Target(target.to_string()))?;
    let rest = rest.trim_start_matches("//");
    let (zone, query) = match rest.split_once('?') {
        Some((z, q)) => (z, Some(q)),
        None => (rest, None),
    };
    if zone.is_empty() {
        return Err(CflError::Target(target.to_string()));
    }
    let param = |k: &str| {
        query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(percent_decode))
        })
    };
    let since_secs = match param("since") {
        None => {
            return Err(CflError::Target(format!(
                "{target}: no since= — the edge keeps a rolling window, \
                 name the slice you want"
            )));
        }
        Some(s) => {
            if let Some(d) = duration_secs(&s) {
                now_secs - d
            } else if let Some((secs, ..)) = parse_iso(&s) {
                secs
            } else {
                return Err(CflError::Target(format!(
                    "since={s}: not a duration (30m, 1h, 2d) or an ISO instant"
                )));
            }
        }
    };
    let until_secs = match param("until") {
        None => None,
        Some(u) => match parse_iso(&u) {
            Some((secs, ..)) => Some(secs),
            None => return Err(CflError::Target(format!("until={u}: not an ISO instant"))),
        },
    };
    let limit = match param("limit") {
        None => None,
        Some(l) => Some(
            l.parse::<u64>()
                .map_err(|_| CflError::Target(format!("limit={l}: not a number")))?,
        ),
    };
    Ok((
        Target {
            zone: zone.to_string(),
            since_secs,
            until_secs,
            limit,
            fields: param("fields").unwrap_or_else(|| DEFAULT_FIELDS.to_string()),
        },
        param("endpoint"),
    ))
}

/// Format an epoch as the RFC 3339 instant Logpull wants.
fn rfc3339(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = quarb::temporal::components(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Decode one NDJSON record.
fn decode_rec(o: &Json) -> Option<Rec> {
    let mut secs = 0i64;
    let mut nanos = 0u32;
    let mut fields: Vec<(String, Value)> = Vec::new();
    for (k, v) in o.as_object()? {
        let val = match v {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            Json::String(s) => {
                if s.contains('T')
                    && let Some((sec, nano, offset_min)) = parse_iso(s)
                {
                    Value::Instant {
                        secs: sec,
                        nanos: nano,
                        offset_min,
                    }
                } else {
                    Value::Str(s.clone())
                }
            }
            other => Value::Str(other.to_string()),
        };
        if k == "EdgeStartTimestamp" {
            match &val {
                Value::Instant { secs: s, nanos: n, .. } => {
                    secs = *s;
                    nanos = *n;
                }
                // Integer nanosecond epochs, Logpull's unixnano mode.
                Value::Int(ns) => {
                    secs = ns.div_euclid(1_000_000_000);
                    nanos = ns.rem_euclid(1_000_000_000) as u32;
                }
                _ => {}
            }
        }
        fields.push((k.clone(), val));
    }
    Some(Rec { secs, nanos, fields })
}

impl CflAdapter {
    /// Open a bounded snapshot of the zone's request records.
    pub fn open(target: &str) -> Result<Self, CflError> {
        // The pinned invocation instant (`qua --now`, else the
        // clock read once at startup) resolves `since=`, so a
        // pinned run's window is reproducible.
        let now_secs = quarb::now_secs();
        let (t, endpoint) = parse_target(target, now_secs)?;
        let token = std::env::var("CLOUDFLARE_API_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                CflError::Api("no $CLOUDFLARE_API_TOKEN in the environment".into())
            })?;
        let base = endpoint.unwrap_or_else(|| "https://api.cloudflare.com".to_string());
        // Logpull refuses ranges that reach into the last minute.
        let until = t.until_secs.unwrap_or(now_secs - 65).min(now_secs - 65);
        let mut url = format!(
            "{}/client/v4/zones/{}/logs/received?start={}&end={}&timestamps=rfc3339&fields={}",
            base.trim_end_matches('/'),
            t.zone,
            rfc3339(t.since_secs),
            rfc3339(until),
            t.fields,
        );
        if let Some(l) = t.limit {
            url.push_str(&format!("&count={l}"));
        }
        let resp = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", token.trim()))
            .call()
            .map_err(|e| CflError::Api(format!("logs/received: {e}")))?;
        let text = resp
            .into_string()
            .map_err(|e| CflError::Api(format!("logs/received: {e}")))?;
        let recs: Vec<Rec> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Json>(l).ok())
            .filter_map(|o| decode_rec(&o))
            .collect();
        Ok(Self::from_recs(&t.zone, recs))
    }

    fn from_recs(zone: &str, mut recs: Vec<Rec>) -> Self {
        recs.sort_by(|a, b| (a.secs, a.nanos).cmp(&(b.secs, b.nanos)));
        let adapter = CflAdapter {
            zone: zone.to_string(),
            recs,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        let ids: Vec<NodeId> = (0..adapter.recs.len())
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

    /// Build from already-decoded NDJSON objects — the test
    /// fixture's entry point.
    pub fn from_json(zone: &str, docs: &[Json]) -> Self {
        Self::from_recs(zone, docs.iter().filter_map(decode_rec).collect())
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// A human-readable locator: `/entry[N]`.
    pub fn locator(&self, node: NodeId) -> String {
        match self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => "/".into(),
            Kind::Entry(i) => format!("/entry[{}]", i + 1),
        }
    }

    fn entry_of(&self, node: NodeId) -> Option<usize> {
        match self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entry(i) => Some(i),
            _ => None,
        }
    }
}

impl AstAdapter for CflAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => self.nodes.borrow()[0].children.borrow().clone().unwrap_or_default(),
            Kind::Entry(_) => Vec::new(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.entry_of(node) {
            Some(_) => vec!["entry".into()],
            None => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let i = self.entry_of(node)?;
        let rec = &self.recs[i];
        let get = |k: &str| {
            rec.fields
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        match name {
            "ray" => get("RayID"),
            "timestamp" => Some(Value::Instant {
                secs: rec.secs,
                nanos: rec.nanos,
                offset_min: None,
            }),
            _ => get(name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let i = self.entry_of(node)?;
        let rec = &self.recs[i];
        let get = |k: &str| {
            rec.fields
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        let method = get("ClientRequestMethod")?;
        let host = get("ClientRequestHost")
            .map(|h| h.to_string())
            .unwrap_or_default();
        let uri = get("ClientRequestURI")
            .map(|u| u.to_string())
            .unwrap_or_default();
        let status = get("EdgeResponseStatus")
            .map(|s| format!(" {s}"))
            .unwrap_or_default();
        Some(Value::Str(format!("{method} {host}{uri}{status}")))
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let i = self.entry_of(node)?;
        match key {
            "zone" => Some(Value::Str(self.zone.clone())),
            "cache" => self.recs[i]
                .fields
                .iter()
                .find(|(k, _)| k == "CacheCacheStatus")
                .map(|(_, v)| v.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<Json> {
        vec![
            serde_json::json!({
                "RayID": "8a1b2c3d4e5f0001",
                "EdgeStartTimestamp": "2026-07-25T14:02:07.480Z",
                "ClientRequestMethod": "POST",
                "ClientRequestHost": "shop.example",
                "ClientRequestURI": "/pay",
                "EdgeResponseStatus": 500,
                "OriginResponseStatus": 500,
                "OriginResponseTime": 2912000000i64,
                "CacheCacheStatus": "dynamic"
            }),
            serde_json::json!({
                "RayID": "8a1b2c3d4e5f0000",
                "EdgeStartTimestamp": "2026-07-25T14:00:01.100Z",
                "ClientRequestMethod": "GET",
                "ClientRequestHost": "shop.example",
                "ClientRequestURI": "/catalog",
                "EdgeResponseStatus": 200,
                "OriginResponseStatus": 0,
                "OriginResponseTime": 0,
                "CacheCacheStatus": "hit"
            }),
        ]
    }

    #[test]
    fn since_is_mandatory() {
        assert!(matches!(parse_target("cfl:zone9", 1_000_000), Err(CflError::Target(_))));
        assert!(matches!(
            parse_target("cfl:zone9?limit=100", 1_000_000),
            Err(CflError::Target(_))
        ));
        let (t, _) = parse_target("cfl:zone9?since=1h&limit=100", 1_000_000).unwrap();
        assert_eq!(t.since_secs, 1_000_000 - 3600);
        assert!(t.fields.contains("RayID"));
    }

    #[test]
    fn records_query_and_synthesize() {
        let a = CflAdapter::from_json("zone9", &fixture());
        let run = |q: &str| match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
        };
        // Oldest first; :: is the request line.
        assert_eq!(
            run("/entry::"),
            vec!["GET shop.example/catalog 200", "POST shop.example/pay 500"]
        );
        // Fields are properties; ::ray is the join key; the cache
        // status rides metadata.
        assert_eq!(run("/entry[::EdgeResponseStatus = 500]::ray"), vec!["8a1b2c3d4e5f0001"]);
        assert_eq!(run("/entry[::::cache = 'hit'] @| count"), vec!["1"]);
        // The typed instant.
        assert_eq!(run("/entry[::timestamp > 2026-07-25] @| count"), vec!["2"]);
    }
}
