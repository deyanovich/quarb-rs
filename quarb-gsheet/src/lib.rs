//! Google Sheets adapter for the Quarb query engine.
//!
//! A spreadsheet in the cloud, on the same mapping as
//! `quarb-xlsx`: sheets as tables, the first row as column
//! headers (positional `cN` for blank or duplicate cells), rows
//! named by their sheet row number, values typed
//! (`valueRenderOption=UNFORMATTED_VALUE`, so numbers arrive
//! numeric). Whole-spreadsheet eager load — one metadata call
//! plus one values call per sheet.
//!
//! **Auth**: the Sheets API does not accept `cloud-platform`
//! tokens (gcloud's default scope), so the friction-free path is
//! an **API key** (`?key=...` on the target, else
//! `QUARB_GSHEET_KEY`) — enough for any sheet readable by link.
//! A properly scoped bearer token (`QUARB_GCP_TOKEN` holding a
//! `spreadsheets.readonly`-scoped token) works for private
//! sheets. Read-only, as always.
//!
//! **Target**: `gsheet://SPREADSHEET_ID[?key=APIKEY]` — the id is
//! the long segment of the sheet's URL.

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use serde_json::Value as Json;

/// An error connecting to a spreadsheet.
#[derive(Debug, thiserror::Error)]
pub enum GsheetError {
    #[error("gsheet: {0}")]
    Api(String),
    #[error("gsheet target: {0} (expected gsheet://SPREADSHEET_ID[?key=APIKEY])")]
    Target(String),
}

/// A spreadsheet, exposed as an arbor of sheets and rows.
pub struct GsheetAdapter {
    model: RelationalModel,
}

/// Redact the API key from a ureq error string. ureq's `Error`
/// Display prefixes the full request URL — which carries the
/// `?key=...` credential — onto both status and transport errors,
/// so the key must be scrubbed before the message reaches a user
/// (terminal or logs). Empty keys are a no-op to avoid replacing
/// every gap in the string.
fn scrub_key(msg: String, key: Option<&str>) -> String {
    match key {
        Some(k) if !k.is_empty() => msg.replace(k, "REDACTED"),
        _ => msg,
    }
}

fn cell_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        Json::String(s) if s.is_empty() => Value::Null,
        Json::String(s) => Value::Str(s.clone()),
        other => Value::Str(other.to_string()),
    }
}

impl GsheetAdapter {
    /// Connect to `gsheet://SPREADSHEET_ID[?key=APIKEY]`.
    pub fn connect(target: &str) -> Result<Self, GsheetError> {
        let rest = target
            .strip_prefix("gsheet://")
            .ok_or_else(|| GsheetError::Target(target.to_string()))?;
        let (id, query) = match rest.split_once('?') {
            Some((i, q)) => (i, Some(q)),
            None => (rest, None),
        };
        if id.is_empty() {
            return Err(GsheetError::Target(target.to_string()));
        }
        let key = query
            .and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("key=").map(str::to_string))
            })
            .or_else(|| {
                std::env::var("QUARB_GSHEET_KEY")
                    .ok()
                    .filter(|k| !k.is_empty())
            });
        let bearer = std::env::var("QUARB_GCP_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());

        let get = |path: &str| -> Result<Json, GsheetError> {
            let mut url = format!("https://sheets.googleapis.com/v4/spreadsheets/{id}{path}");
            if let Some(k) = &key {
                url.push_str(if url.contains('?') { "&" } else { "?" });
                url.push_str(&format!("key={k}"));
            }
            let mut req = ureq::get(&url);
            if let Some(t) = &bearer {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            let resp: Json = req
                .call()
                .map_err(|e| GsheetError::Api(scrub_key(e.to_string(), key.as_deref())))?
                .into_json()
                .map_err(|e| GsheetError::Api(format!("decoding response: {e}")))?;
            if let Some(err) = resp.pointer("/error/message").and_then(|v| v.as_str()) {
                return Err(GsheetError::Api(err.to_string()));
            }
            Ok(resp)
        };

        // Sheet titles.
        let meta = get("?fields=sheets.properties.title")?;
        let titles: Vec<String> = meta
            .pointer("/sheets")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.pointer("/properties/title"))
                    .filter_map(|t| t.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let mut tables = Vec::new();
        for title in titles {
            let resp = get(&format!(
                "/values/{}?valueRenderOption=UNFORMATTED_VALUE",
                urlencode(&title)
            ))?;
            let empty = Vec::new();
            let grid = resp
                .pointer("/values")
                .and_then(|v| v.as_array())
                .unwrap_or(&empty);
            let mut it = grid.iter();
            let mut columns: Vec<String> = Vec::new();
            if let Some(head) = it.next().and_then(|r| r.as_array()) {
                for (i, c) in head.iter().enumerate() {
                    let raw = match c {
                        Json::String(s) => s.trim().to_string(),
                        Json::Null => String::new(),
                        other => other.to_string().trim_matches('"').to_string(),
                    };
                    let name = if raw.is_empty() || columns.contains(&raw) {
                        format!("c{}", i + 1)
                    } else {
                        raw
                    };
                    columns.push(name);
                }
            }
            let mut rows = Vec::new();
            for (i, r) in it.enumerate() {
                let mut values: Vec<Value> = r
                    .as_array()
                    .map(|a| a.iter().map(cell_value).collect())
                    .unwrap_or_default();
                values.resize(columns.len(), Value::Null);
                rows.push(RowSpec {
                    rowid: i as i64 + 2,
                    values,
                });
            }
            tables.push((
                TableSpec {
                    name: title,
                    columns,
                    pk: None,
                    fks: Vec::new(),
                },
                rows,
            ));
        }
        Ok(GsheetAdapter {
            model: RelationalModel::build(tables),
        })
    }

    /// A human-readable locator: `/sheet/rownum`.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

impl AstAdapter for GsheetAdapter {
    fn root(&self) -> NodeId {
        self.model.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.model.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.model.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.model.parent(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.model.property(node, name)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.model.metadata(node, key)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.model.resolve(node, property, hint)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.backlinks(node)
    }
}

#[cfg(test)]
mod tests {
    use super::scrub_key;

    #[test]
    fn scrub_key_redacts_the_api_key() {
        let key = "AIzaSyEXAMPLEKEY0123456789";
        // Mirrors ureq's Display: full request URL prefixed onto a status error.
        let msg = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/BADID?fields=sheets.properties.title&key={key}: status code 404"
        );
        let out = scrub_key(msg, Some(key));
        assert!(!out.contains(key), "api key leaked: {out}");
        assert!(out.contains("REDACTED"));
        assert!(out.contains("status code 404"));
    }

    #[test]
    fn scrub_key_is_noop_without_a_key() {
        let msg = "connection refused".to_string();
        assert_eq!(scrub_key(msg.clone(), None), msg);
        // An empty key must not splatter REDACTED into every gap.
        assert_eq!(scrub_key(msg.clone(), Some("")), msg);
    }
}
