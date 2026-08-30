//! Azure Monitor Logs adapter for the Quarb query engine.
//!
//! The logs-family design sheet, over Log Analytics: a mount is
//! a *bounded snapshot* — `since=` and/or `limit=` are mandatory
//! — of one or more workspace tables, fetched through the KQL
//! query endpoint. The adapter writes the KQL itself (a table
//! scan with the time bound, your `filter=` as a `| where`
//! clause, and the limit); you never leave query-language home.
//!
//! One table in the target puts its rows at the root as
//! `/entry` children, oldest first; a comma list mounts each
//! table as a root child. Column values become properties by
//! column name — `datetime` columns as typed instants, `dynamic`
//! (JSON) columns decoded into attribute subtrees with the
//! family's property fallthrough. The default value (`::`) is
//! the row's `Message` / `message` column when the table has
//! one, or a synthesized request line (`Name` + `ResultCode`,
//! the AppRequests shape).
//!
//! - **Metadata**: `;;;table`, `;;;workspace`, `;;;type` (the
//!   row's `Type` column when present).
//! - **The join**: Azure threads `OperationId` through
//!   application tables — the family's correlation key on this
//!   provider (`::operation_Id`/`::OperationId` fall through
//!   like everything else).
//!
//! **Transport and auth**: `POST /v1/workspaces/{id}/query`
//! with an AAD bearer token — from `$AZURE_LOG_TOKEN`, or `az
//! account get-access-token --resource https://api.loganalytics.io`
//! when the az CLI is present (`QUARB_AZ` overrides the
//! binary). `endpoint=` overrides the URL (the bottled test
//! server).
//!
//! **Target**:
//! `azl:WORKSPACE_ID?table=AppRequests[,AppTraces]&since=1h&until=…&filter=…&limit=N&endpoint=…`
//! (`azlogs:` works too). `filter` is a raw KQL boolean
//! expression, applied server-side.

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use serde_json::json;
use std::cell::RefCell;

/// An error connecting to or reading Azure Monitor Logs.
#[derive(Debug, thiserror::Error)]
pub enum AzlError {
    #[error("azure monitor: {0}")]
    Api(String),
    #[error(
        "azlogs target: {0} (expected \
         azl:WORKSPACE?table=AppRequests&since=1h&limit=N; a mount \
         must name its table(s) and be bounded — give it since= \
         and/or limit=)"
    )]
    Target(String),
}

/// One decoded cell tree: a JSON attribute tree or a scalar.
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

/// One row, decoded against its table's columns.
#[derive(Clone)]
struct Row {
    secs: i64,
    nanos: u32,
    /// (column name, decoded value) in column order.
    cells: Vec<(String, Field)>,
}

enum Kind {
    Root,
    /// A mounted table's slot in `tables`.
    Table(usize),
    Entry { table: usize, idx: usize },
    Field(Field),
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A bounded snapshot of Log Analytics tables.
pub struct AzlAdapter {
    workspace: String,
    tables: Vec<(String, Vec<Row>)>,
    nodes: RefCell<Vec<Node>>,
}

/// A `since=` duration in seconds, for resolving a relative
/// window against the pinned invocation instant.
fn span_secs(s: &str) -> Option<i64> {
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

/// A KQL timespan for `since=` durations: `30m` → `PT30M`,
/// `2d` → `P2D`.
fn kql_ago(s: &str) -> Option<String> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = num.parse().ok()?;
    Some(match unit {
        "s" => format!("{n}s"),
        "m" => format!("{n}m"),
        "h" => format!("{n}h"),
        "d" => format!("{n}d"),
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
    workspace: String,
    tables: Vec<String>,
    since: Option<String>,
    until: Option<String>,
    filter: Option<String>,
    limit: Option<u64>,
}

fn parse_target(target: &str) -> Result<(Target, Option<String>), AzlError> {
    let rest = target
        .strip_prefix("azl:")
        .or_else(|| target.strip_prefix("azlogs:"))
        .ok_or_else(|| AzlError::Target(target.to_string()))?;
    let rest = rest.trim_start_matches("//");
    let (workspace, query) = match rest.split_once('?') {
        Some((w, q)) => (w, Some(q)),
        None => (rest, None),
    };
    if workspace.is_empty() {
        return Err(AzlError::Target(target.to_string()));
    }
    let param = |k: &str| {
        query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(percent_decode))
        })
    };
    let tables: Vec<String> = param("table")
        .map(|t| t.split(',').map(str::to_string).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    if tables.is_empty() {
        return Err(AzlError::Target(format!(
            "{target}: no table= — Log Analytics queries name a table \
             (AppRequests, AppTraces, AzureDiagnostics, …)"
        )));
    }
    let since = param("since");
    if let Some(s) = &since
        && kql_ago(s).is_none()
        && parse_iso(s).is_none()
    {
        return Err(AzlError::Target(format!(
            "since={s}: not a duration (30m, 1h, 2d) or an ISO instant"
        )));
    }
    let until = param("until");
    if let Some(u) = &until
        && parse_iso(u).is_none()
    {
        return Err(AzlError::Target(format!("until={u}: not an ISO instant")));
    }
    let limit = match param("limit") {
        None => None,
        Some(l) => Some(
            l.parse::<u64>()
                .map_err(|_| AzlError::Target(format!("limit={l}: not a number")))?,
        ),
    };
    if since.is_none() && limit.is_none() {
        return Err(AzlError::Target(format!(
            "{target}: unbounded — a busy workspace holds more rows than \
             you want to snapshot"
        )));
    }
    Ok((
        Target {
            workspace: workspace.to_string(),
            tables,
            since,
            until,
            filter: param("filter"),
            limit,
        },
        param("endpoint"),
    ))
}

/// Compose the KQL for one table: the bounded scan.
fn compose_kql(t: &Target, table: &str) -> String {
    let mut q = table.to_string();
    // Pinned invocation instant: a relative window becomes an
    // absolute datetime bound, so the workspace answers against
    // *our* instant and a pinned run replays. Unpinned, `ago()`
    // rides the provider's clock as before.
    let pinned = quarb::invocation_instant().map(|(secs, _)| secs);
    if let Some(s) = &t.since {
        if let (Some(now), Some(d)) = (pinned, span_secs(s)) {
            let at = quarb::temporal::format_instant(now - d, 0, Some(0));
            q.push_str(&format!(" | where TimeGenerated >= datetime({at})"));
        } else if let Some(span) = kql_ago(s) {
            q.push_str(&format!(" | where TimeGenerated >= ago({span})"));
        } else {
            q.push_str(&format!(" | where TimeGenerated >= datetime({s})"));
        }
    }
    if let Some(u) = &t.until {
        q.push_str(&format!(" | where TimeGenerated <= datetime({u})"));
    }
    if let Some(f) = &t.filter
        && !f.trim().is_empty()
    {
        q.push_str(&format!(" | where {}", f.trim()));
    }
    q.push_str(" | sort by TimeGenerated asc");
    if let Some(l) = t.limit {
        q.push_str(&format!(" | take {l}"));
    }
    q
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

/// Decode one KQL result table (columns + row arrays) into rows.
fn decode_table(t: &Json) -> Vec<Row> {
    let cols: Vec<(String, String)> = t
        .pointer("/columns")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some((
                        c.pointer("/name")?.as_str()?.to_string(),
                        c.pointer("/type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("string")
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut rows: Vec<Row> = Vec::new();
    for r in t
        .pointer("/rows")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let Some(vals) = r.as_array() else { continue };
        let mut secs = 0i64;
        let mut nanos = 0u32;
        let mut cells: Vec<(String, Field)> = Vec::new();
        for ((name, ty), v) in cols.iter().zip(vals) {
            let field = match ty.as_str() {
                // A `dynamic` column is JSON — sometimes as a
                // string, sometimes inline.
                "dynamic" => match v {
                    Json::String(s) => serde_json::from_str::<Json>(s)
                        .map(|d| decode_json(&d))
                        .unwrap_or_else(|_| Field::Scalar(str_value(s))),
                    other => decode_json(other),
                },
                _ => decode_json(v),
            };
            if name == "TimeGenerated"
                && let Some(Value::Instant { secs: s, nanos: n, .. }) = field.scalar()
            {
                secs = s;
                nanos = n;
            }
            cells.push((name.clone(), field));
        }
        rows.push(Row { secs, nanos, cells });
    }
    rows.sort_by(|a, b| (a.secs, a.nanos).cmp(&(b.secs, b.nanos)));
    rows
}

/// The AAD bearer token: `$AZURE_LOG_TOKEN`, or the az CLI.
fn bearer_token() -> Result<String, AzlError> {
    if let Ok(t) = std::env::var("AZURE_LOG_TOKEN")
        && !t.trim().is_empty()
    {
        return Ok(t.trim().to_string());
    }
    let bin = std::env::var("QUARB_AZ").unwrap_or_else(|_| "az".into());
    let out = std::process::Command::new(&bin)
        .args([
            "account",
            "get-access-token",
            "--resource",
            "https://api.loganalytics.io",
            "--query",
            "accessToken",
            "-o",
            "tsv",
        ])
        .output()
        .map_err(|e| {
            AzlError::Api(format!(
                "no token: $AZURE_LOG_TOKEN unset and running {bin} failed: {e}"
            ))
        })?;
    if !out.status.success() {
        return Err(AzlError::Api(format!(
            "az account get-access-token: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl AzlAdapter {
    /// Open a bounded snapshot of the target's tables.
    pub fn open(target: &str) -> Result<Self, AzlError> {
        let (t, endpoint) = parse_target(target)?;
        let token = bearer_token()?;
        let base = endpoint
            .unwrap_or_else(|| "https://api.loganalytics.io".to_string());
        let url = format!(
            "{}/v1/workspaces/{}/query",
            base.trim_end_matches('/'),
            t.workspace
        );
        let mut tables: Vec<(String, Vec<Row>)> = Vec::new();
        for table in &t.tables {
            let body = json!({ "query": compose_kql(&t, table) });
            let resp = ureq::post(&url)
                .set("Authorization", &format!("Bearer {token}"))
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
                .map_err(|e| AzlError::Api(format!("{table}: {e}")))?;
            let text = resp
                .into_string()
                .map_err(|e| AzlError::Api(format!("{table}: {e}")))?;
            let doc: Json = serde_json::from_str(&text)
                .map_err(|e| AzlError::Api(format!("{table}: {e}")))?;
            let rows = doc
                .pointer("/tables")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .map(decode_table)
                .unwrap_or_default();
            tables.push((table.clone(), rows));
        }
        Ok(Self::build(&t.workspace, tables))
    }

    /// Build the arbor from externally decoded rows — the test
    /// fixture's entry point.
    pub fn from_tables(workspace: &str, tables: Vec<(String, Vec<Row2>)>) -> Self {
        let tables: Vec<(String, Vec<Row>)> = tables
            .into_iter()
            .map(|(n, rows)| (n, rows.into_iter().map(Into::into).collect()))
            .collect();
        Self::build(workspace, tables)
    }

    fn build(workspace: &str, tables: Vec<(String, Vec<Row>)>) -> Self {
        let adapter = AzlAdapter {
            workspace: workspace.to_string(),
            tables,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
        };
        let root_kids: Vec<NodeId> = if adapter.tables.len() == 1 {
            (0..adapter.tables[0].1.len())
                .map(|idx| {
                    adapter.push(Node {
                        kind: Kind::Entry { table: 0, idx },
                        name: Some("entry".into()),
                        parent: Some(NodeId(0)),
                        children: RefCell::new(None),
                    })
                })
                .collect()
        } else {
            (0..adapter.tables.len())
                .map(|ti| {
                    let name = adapter.tables[ti].0.clone();
                    adapter.push(Node {
                        kind: Kind::Table(ti),
                        name: Some(name),
                        parent: Some(NodeId(0)),
                        children: RefCell::new(None),
                    })
                })
                .collect()
        };
        *adapter.nodes.borrow()[0].children.borrow_mut() = Some(root_kids);
        adapter
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// A human-readable locator: `[Table/]entry[N]/…`.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let nodes = self.nodes.borrow();
            let n = &nodes[id.0 as usize];
            match &n.kind {
                Kind::Root => {}
                Kind::Entry { idx, .. } => parts.push(format!("entry[{}]", idx + 1)),
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
            Table(usize),
            Entry(usize, usize),
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
                    Kind::Table(ti) => Plan::Table(*ti),
                    Kind::Entry { table, idx } => Plan::Entry(*table, *idx),
                    Kind::Field(Field::Map(entries)) => Plan::Fields(entries.clone()),
                    Kind::Field(Field::List(items)) => Plan::Items(items.clone()),
                    Kind::Field(Field::Scalar(_)) => Plan::Leaf,
                }
            }
        };
        let made = match plan {
            Plan::Done(k) => return k,
            Plan::Leaf => Vec::new(),
            Plan::Table(ti) => (0..self.tables[ti].1.len())
                .map(|idx| {
                    self.push(Node {
                        kind: Kind::Entry { table: ti, idx },
                        name: Some("entry".into()),
                        parent: Some(node),
                        children: RefCell::new(None),
                    })
                })
                .collect(),
            Plan::Entry(t, i) => {
                // Children: only the dynamic (tree-shaped) cells —
                // scalar columns stay properties.
                let cells = self.tables[t].1[i].cells.clone();
                cells
                    .into_iter()
                    .filter(|(_, f)| !matches!(f, Field::Scalar(_)))
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

    fn entry_at(&self, node: NodeId) -> Option<(usize, usize)> {
        match self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entry { table, idx } => Some((table, idx)),
            _ => None,
        }
    }
}

/// The public row shape for [`AzlAdapter::from_tables`] — what a
/// test fixture builds without touching the private decode.
pub struct Row2 {
    pub secs: i64,
    pub nanos: u32,
    pub cells: Vec<(String, serde_json::Value)>,
}

impl From<Row2> for Row {
    fn from(r: Row2) -> Row {
        Row {
            secs: r.secs,
            nanos: r.nanos,
            cells: r
                .cells
                .into_iter()
                .map(|(k, v)| (k, decode_json(&v)))
                .collect(),
        }
    }
}

impl AstAdapter for AzlAdapter {
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
            Kind::Table(_) => vec!["table".into()],
            Kind::Entry { .. } => vec!["entry".into()],
            _ => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if let Some((t, i)) = self.entry_at(node) {
            let row = &self.tables[t].1[i];
            if name == "timestamp" {
                return Some(Value::Instant {
                    secs: row.secs,
                    nanos: row.nanos,
                    offset_min: None,
                });
            }
            if let Some(v) = row
                .cells
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, f)| f.scalar())
            {
                return Some(v);
            }
            // Fallthrough into dynamic cells' top level.
            for (_, f) in &row.cells {
                if let Field::Map(fields) = f
                    && let Some(v) = fields
                        .iter()
                        .find(|(k, _)| k == name)
                        .and_then(|(_, f)| f.scalar())
                {
                    return Some(v);
                }
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
        if let Some((t, i)) = self.entry_at(node) {
            let row = &self.tables[t].1[i];
            let get = |k: &str| {
                row.cells
                    .iter()
                    .find(|(key, _)| key == k)
                    .and_then(|(_, f)| f.scalar())
            };
            for k in ["Message", "message"] {
                if let Some(v) = get(k) {
                    return Some(v);
                }
            }
            // The AppRequests shape: name + result code.
            if let Some(n) = get("Name") {
                let code = get("ResultCode").map(|c| format!(" {c}")).unwrap_or_default();
                return Some(Value::Str(format!("{n}{code}")));
            }
            return None;
        }
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field(f) => f.scalar(),
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let (t, i) = self.entry_at(node)?;
        match key {
            "table" => Some(Value::Str(self.tables[t].0.clone())),
            "workspace" => Some(Value::Str(self.workspace.clone())),
            "type" => self.tables[t].1[i]
                .cells
                .iter()
                .find(|(k, _)| k == "Type")
                .and_then(|(_, f)| f.scalar()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_and_bound_are_enforced() {
        assert!(matches!(parse_target("azl:ws-1?since=1h"), Err(AzlError::Target(_))));
        assert!(matches!(
            parse_target("azl:ws-1?table=AppRequests"),
            Err(AzlError::Target(_))
        ));
        assert!(parse_target("azl:ws-1?table=AppRequests&since=1h").is_ok());
        assert!(parse_target("azlogs:ws-1?table=A,B&limit=50").is_ok());
    }

    #[test]
    fn kql_composition() {
        let (t, _) = parse_target(
            "azl:ws?table=AppRequests&since=1h&filter=Success%20==%20false&limit=100",
        )
        .unwrap();
        assert_eq!(
            compose_kql(&t, "AppRequests"),
            "AppRequests | where TimeGenerated >= ago(1h) \
             | where Success == false | sort by TimeGenerated asc | take 100"
        );
        let (t, _) =
            parse_target("azl:ws?table=T&since=2026-07-25T00:00:00Z&until=2026-07-25T12:00:00Z")
                .unwrap();
        assert_eq!(
            compose_kql(&t, "T"),
            "T | where TimeGenerated >= datetime(2026-07-25T00:00:00Z) \
             | where TimeGenerated <= datetime(2026-07-25T12:00:00Z) \
             | sort by TimeGenerated asc"
        );
    }

    #[test]
    fn rows_decode_and_query() {
        let doc: Json = serde_json::json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    {"name": "TimeGenerated", "type": "datetime"},
                    {"name": "Name", "type": "string"},
                    {"name": "ResultCode", "type": "string"},
                    {"name": "DurationMs", "type": "real"},
                    {"name": "Properties", "type": "dynamic"},
                    {"name": "OperationId", "type": "string"}
                ],
                "rows": [
                    ["2026-07-25T14:02:07.5Z", "POST /pay", "500", 2912.0,
                     "{\"order\": \"o-1402\", \"region\": \"eu\"}", "77cc41090e"],
                    ["2026-07-25T14:00:02.3Z", "POST /pay", "200", 87.0,
                     "{\"order\": \"o-1401\", \"region\": \"eu\"}", "9ab2277d10"]
                ]
            }]
        });
        let rows = decode_table(&doc.pointer("/tables/0").unwrap());
        let a = AzlAdapter::build("ws-demo", vec![("AppRequests".into(), rows)]);
        let run = |q: &str| match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
        };
        // Oldest first regardless of response order; :: synthesizes
        // the request line.
        assert_eq!(run("/entry::"), vec!["POST /pay 200", "POST /pay 500"]);
        // Columns are properties; dynamic cells fall through.
        assert_eq!(run("/entry[::DurationMs > 1000]::order"), vec!["o-1402"]);
        // TimeGenerated is a typed instant behind ::timestamp.
        assert_eq!(run("/entry[::timestamp > 2026-07-25] @| count"), vec!["2"]);
        // Metadata names the table.
        assert_eq!(run("/entry[1]::::table"), vec!["AppRequests"]);
    }
}
