//! Object-store adapter for the Quarb query engine: Google Cloud
//! Storage and Amazon S3 buckets as lazy directory trees.
//!
//! Object keys with `/` separators span the tree the way a
//! filesystem does — the adapter lists one "directory" per touch
//! (delimiter listing, paginated), and an object's content is its
//! value, fetched on first read and cached. Under composition
//! (`qua` wraps object stores by default), a bucket of JSON, CSV,
//! or source files is directly queryable: the object is a leaf,
//! its parsed content the subtree — grafting is the point of this
//! adapter.
//!
//! **Targets**:
//! - `gs://BUCKET[/PREFIX]` — GCS, JSON API. Public buckets work
//!   anonymously; private ones authenticate like the other GCP
//!   drivers (`QUARB_GCP_TOKEN`, else `gcloud auth
//!   print-access-token`, `?account=EMAIL` to pick the account —
//!   set `?auth=1` to force a token for non-public buckets).
//! - `s3://BUCKET[/PREFIX][?region=R]` — S3, ListObjectsV2.
//!   **Anonymous only in v1**: public buckets read without
//!   credentials; SigV4 request signing is a recorded extension,
//!   so private S3 buckets refuse honestly rather than
//!   half-work.
//!
//! Metadata: `;;;size`, `;;;updated` on objects; traits
//! `<object>` / `<prefix>`. Read-only, as always.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;

/// An error connecting to a bucket.
#[derive(Debug, thiserror::Error)]
pub enum ObjstoreError {
    #[error("objstore: {0}")]
    Http(String),
    #[error("objstore target: {0} (expected gs://BUCKET[/PREFIX] or s3://BUCKET[/PREFIX])")]
    Target(String),
}

enum Backend {
    Gcs { token: Option<String> },
    S3 { region: String },
}

struct Node {
    /// Full key prefix (dirs end without `/`; root is "").
    key: String,
    name: Option<String>,
    parent: Option<NodeId>,
    is_object: bool,
    size: Option<i64>,
    updated: Option<String>,
    children: RefCell<Option<Vec<NodeId>>>,
    content: RefCell<Option<String>>,
}

/// A bucket (or prefix of one), exposed as an arbor.
pub struct ObjstoreAdapter {
    backend: Backend,
    bucket: String,
    /// The target's prefix, "" for the whole bucket.
    base: String,
    nodes: RefCell<Vec<Node>>,
}

fn gcp_token(account: Option<&str>) -> Option<String> {
    if let Ok(t) = std::env::var("QUARB_GCP_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    let mut cmd = std::process::Command::new("gcloud");
    cmd.args(["auth", "print-access-token"]);
    if let Some(a) = account {
        cmd.arg(a);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
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

/// One listing page: (dir prefixes, objects as (key, size, updated)).
type Page = (Vec<String>, Vec<(String, Option<i64>, Option<String>)>);

impl ObjstoreAdapter {
    /// Connect to `gs://...` or `s3://...`; one listing probes the
    /// bucket.
    pub fn connect(target: &str) -> Result<Self, ObjstoreError> {
        let (backend, rest) = if let Some(r) = target.strip_prefix("gs://") {
            ("gs", r)
        } else if let Some(r) = target.strip_prefix("s3://") {
            ("s3", r)
        } else {
            return Err(ObjstoreError::Target(target.to_string()));
        };
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let (bucket, prefix) = match path.split_once('/') {
            Some((b, p)) => (b.to_string(), p.trim_end_matches('/').to_string()),
            None => (path.to_string(), String::new()),
        };
        if bucket.is_empty() {
            return Err(ObjstoreError::Target(target.to_string()));
        }
        let param = |k: &str| {
            query.and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            })
        };
        let backend = if backend == "gs" {
            let token = if param("auth").is_some() || param("account").is_some() {
                gcp_token(param("account").as_deref())
            } else {
                None
            };
            Backend::Gcs { token }
        } else {
            Backend::S3 {
                region: param("region").unwrap_or_else(|| "us-east-1".to_string()),
            }
        };
        let adapter = ObjstoreAdapter {
            backend,
            bucket,
            base: prefix.clone(),
            nodes: RefCell::new(vec![Node {
                key: prefix,
                name: None,
                parent: None,
                is_object: false,
                size: None,
                updated: None,
                children: RefCell::new(None),
                content: RefCell::new(None),
            }]),
        };
        adapter
            .list(&adapter.nodes.borrow()[0].key.clone())
            .map_err(|e| ObjstoreError::Http(format!("probing the bucket: {e}")))?;
        Ok(adapter)
    }

    /// A human-readable locator: the object key below the base.
    pub fn locator(&self, node: NodeId) -> String {
        let key = &self.nodes.borrow()[node.0 as usize].key;
        let rel = key.strip_prefix(&self.base).unwrap_or(key);
        format!("/{}", rel.trim_start_matches('/'))
    }

    fn get(&self, url: &str) -> Result<String, String> {
        let mut req = ureq::get(url);
        if let Backend::Gcs { token: Some(t) } = &self.backend {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        req.call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())
    }

    /// One delimiter listing under `prefix`, following pages.
    fn list(&self, prefix: &str) -> Result<Page, String> {
        let dir = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        let mut prefixes = Vec::new();
        let mut objects = Vec::new();
        let mut page: Option<String> = None;
        loop {
            match &self.backend {
                Backend::Gcs { .. } => {
                    let mut url = format!(
                        "https://storage.googleapis.com/storage/v1/b/{}/o?delimiter=/&prefix={}",
                        self.bucket,
                        urlencode(&dir)
                    );
                    if let Some(p) = &page {
                        url.push_str(&format!("&pageToken={}", urlencode(p)));
                    }
                    let resp: serde_json::Value = serde_json::from_str(&self.get(&url)?)
                        .map_err(|e| format!("listing: {e}"))?;
                    if let Some(err) = resp.pointer("/error/message").and_then(|v| v.as_str()) {
                        return Err(err.to_string());
                    }
                    if let Some(ps) = resp.pointer("/prefixes").and_then(|v| v.as_array()) {
                        prefixes.extend(
                            ps.iter()
                                .filter_map(|p| p.as_str())
                                .map(|p| p.trim_end_matches('/').to_string()),
                        );
                    }
                    if let Some(items) = resp.pointer("/items").and_then(|v| v.as_array()) {
                        for i in items {
                            let Some(key) = i.pointer("/name").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            if key.ends_with('/') {
                                continue; // zero-byte "directory" markers
                            }
                            objects.push((
                                key.to_string(),
                                i.pointer("/size")
                                    .and_then(|v| v.as_str())
                                    .and_then(|s| s.parse().ok()),
                                i.pointer("/updated")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                            ));
                        }
                    }
                    page = resp
                        .pointer("/nextPageToken")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                Backend::S3 { region } => {
                    let host = if region == "us-east-1" {
                        format!("{}.s3.amazonaws.com", self.bucket)
                    } else {
                        format!("{}.s3.{region}.amazonaws.com", self.bucket)
                    };
                    let mut url = format!(
                        "https://{host}/?list-type=2&delimiter=/&prefix={}",
                        urlencode(&dir)
                    );
                    if let Some(p) = &page {
                        url.push_str(&format!("&continuation-token={}", urlencode(p)));
                    }
                    let xml = self.get(&url)?;
                    let (ps, os, next) = parse_s3_listing(&xml)?;
                    prefixes.extend(ps);
                    objects.extend(os);
                    page = next;
                }
            }
            if page.is_none() {
                break;
            }
        }
        Ok((prefixes, objects))
    }

    fn push(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(node);
        id
    }

    /// An object's content, fetched once (text, lossily decoded).
    fn content_of(&self, node: NodeId) -> Option<String> {
        if let Some(c) = &*self.nodes.borrow()[node.0 as usize].content.borrow() {
            return Some(c.clone());
        }
        let (key, is_object) = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            (n.key.clone(), n.is_object)
        };
        if !is_object {
            return None;
        }
        let url = match &self.backend {
            Backend::Gcs { .. } => format!(
                "https://storage.googleapis.com/storage/v1/b/{}/o/{}?alt=media",
                self.bucket,
                urlencode(&key)
            ),
            Backend::S3 { region } => {
                let host = if region == "us-east-1" {
                    format!("{}.s3.amazonaws.com", self.bucket)
                } else {
                    format!("{}.s3.{region}.amazonaws.com", self.bucket)
                };
                format!("https://{host}/{}", urlencode(&key).replace("%2F", "/"))
            }
        };
        let text = self.get(&url).ok()?;
        *self.nodes.borrow()[node.0 as usize].content.borrow_mut() = Some(text.clone());
        Some(text)
    }
}

/// Parse an S3 ListObjectsV2 response (streamed, no DOM).
#[allow(clippy::type_complexity)]
fn parse_s3_listing(
    xml: &str,
) -> Result<
    (
        Vec<String>,
        Vec<(String, Option<i64>, Option<String>)>,
        Option<String>,
    ),
    String,
> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut prefixes = Vec::new();
    let mut objects = Vec::new();
    let mut next = None;
    let mut path: Vec<String> = Vec::new();
    let mut cur: (Option<String>, Option<i64>, Option<String>) = (None, None, None);
    loop {
        match reader.read_event().map_err(|e| format!("listing: {e}"))? {
            Event::Start(e) => path.push(String::from_utf8_lossy(e.name().as_ref()).into_owned()),
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "Contents"
                    && let (Some(k), size, updated) = std::mem::take(&mut cur)
                    && !k.ends_with('/')
                {
                    objects.push((k, size, updated));
                }
                path.pop();
            }
            Event::Text(t) => {
                let text = t.xml_content().map_err(|e| e.to_string())?.into_owned();
                match path.as_slice() {
                    [.., a, b] if a == "CommonPrefixes" && b == "Prefix" => {
                        prefixes.push(text.trim_end_matches('/').to_string());
                    }
                    [.., a, b] if a == "Contents" && b == "Key" => cur.0 = Some(text),
                    [.., a, b] if a == "Contents" && b == "Size" => {
                        cur.1 = text.parse().ok();
                    }
                    [.., a, b] if a == "Contents" && b == "LastModified" => {
                        cur.2 = Some(text);
                    }
                    [.., b] if b == "NextContinuationToken" => next = Some(text),
                    [.., a, b] if a == "ListBucketResult" && b == "NextContinuationToken" => {
                        next = Some(text)
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((prefixes, objects, next))
}

impl AstAdapter for ObjstoreAdapter {
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
        let (key, is_object) = {
            let nodes = self.nodes.borrow();
            let n = &nodes[node.0 as usize];
            (n.key.clone(), n.is_object)
        };
        if is_object {
            return Vec::new();
        }
        let (prefixes, objects) = self.list(&key).unwrap_or_default();
        let mut ids = Vec::new();
        for p in prefixes {
            let name = p.rsplit('/').next().unwrap_or(&p).to_string();
            ids.push(self.push(Node {
                key: p,
                name: Some(name),
                parent: Some(node),
                is_object: false,
                size: None,
                updated: None,
                children: RefCell::new(None),
                content: RefCell::new(None),
            }));
        }
        for (k, size, updated) in objects {
            let name = k.rsplit('/').next().unwrap_or(&k).to_string();
            ids.push(self.push(Node {
                key: k,
                name: Some(name),
                parent: Some(node),
                is_object: true,
                size,
                updated,
                children: RefCell::new(None),
                content: RefCell::new(None),
            }));
        }
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// `<object>` / `<prefix>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let nodes = self.nodes.borrow();
        let n = &nodes[node.0 as usize];
        if n.parent.is_none() {
            return Vec::new();
        }
        vec![if n.is_object { "object" } else { "prefix" }.to_string()]
    }

    /// An object's content (fetched on first read, cached).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.content_of(node).map(Value::Str)
    }

    /// `;;;size`, `;;;updated`, `;;;key`.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let nodes = self.nodes.borrow();
        let n = &nodes[node.0 as usize];
        match key {
            "size" => n.size.map(Value::bytes),
            "updated" => n.updated.clone().map(Value::Str),
            "key" => Some(Value::Str(n.key.clone())),
            _ => None,
        }
    }
}
