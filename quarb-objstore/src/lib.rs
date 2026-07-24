//! Object-store adapter for the Quarb query engine: Google Cloud
//! Storage, Amazon S3, and Azure Blob Storage as lazy directory
//! trees.
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
//! - `s3://BUCKET[/PREFIX][?region=R][&endpoint=URL][&anon=1]`
//!   — S3, ListObjectsV2. Requests are SigV4-signed whenever the
//!   standard credential chain resolves (env keys, then
//!   `~/.aws/credentials`; see `quarb-aws`), so private buckets
//!   just work; without credentials — or with `anon=1` — the
//!   request goes out unsigned, which public buckets accept.
//!   `endpoint=URL` points at any S3-compatible store (MinIO,
//!   Cloudflare R2, …) using path-style addressing.
//! - `az://ACCOUNT/CONTAINER[/PREFIX][?endpoint=URL][&sas=TOKEN]`
//!   — Azure Blob Storage. Public containers read anonymously; a
//!   `sas=` token rides every request; otherwise, when
//!   `AZURE_STORAGE_KEY` holds the account key, requests carry a
//!   SharedKey signature. `endpoint=URL` points at Azurite or any
//!   compatible endpoint (path-style, account in the path).
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
    #[error("objstore target: {0} (expected gs://BUCKET[/PREFIX], s3://BUCKET[/PREFIX], or az://ACCOUNT/CONTAINER[/PREFIX])")]
    Target(String),
}

enum Backend {
    Gcs {
        token: Option<String>,
    },
    S3 {
        region: String,
        creds: Option<quarb_aws::Credentials>,
        /// S3-compatible endpoint override (path-style).
        endpoint: Option<String>,
    },
    Azure {
        account: String,
        /// A SAS token (no leading `?`), appended to every URL.
        sas: Option<String>,
        /// The decoded account key, for SharedKey signing.
        key: Option<Vec<u8>>,
        /// Endpoint override (Azurite; path-style with account).
        endpoint: Option<String>,
    },
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
        } else if let Some(r) = target.strip_prefix("az://") {
            ("az", r)
        } else {
            return Err(ObjstoreError::Target(target.to_string()));
        };
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        // Azure targets carry the account before the container.
        let (azure_account, path) = if backend == "az" {
            match path.split_once('/') {
                Some((a, rest)) if !a.is_empty() => (Some(a.to_string()), rest),
                _ => return Err(ObjstoreError::Target(target.to_string())),
            }
        } else {
            (None, path)
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
        } else if backend == "az" {
            Backend::Azure {
                account: azure_account.expect("parsed above"),
                sas: param("sas").map(|s| s.trim_start_matches('?').to_string()),
                key: std::env::var("AZURE_STORAGE_KEY")
                    .ok()
                    .filter(|k| !k.trim().is_empty())
                    .and_then(|k| quarb::base64_decode(k.trim())),
                endpoint: param("endpoint").map(|e| e.trim_end_matches('/').to_string()),
            }
        } else {
            Backend::S3 {
                region: quarb_aws::region(param("region").as_deref()),
                creds: if param("anon").is_some() {
                    None
                } else {
                    quarb_aws::load_credentials()
                },
                endpoint: param("endpoint").map(|e| e.trim_end_matches('/').to_string()),
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
        match &self.backend {
            Backend::Gcs { token: Some(t) } => {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            Backend::S3 {
                region,
                creds: Some(c),
                ..
            } => {
                for (k, v) in quarb_aws::sign(c, "GET", url, region, "s3", b"", &[]) {
                    if k != "host" {
                        req = req.set(&k, &v);
                    }
                }
            }
            Backend::Azure { account, key: Some(k), .. } => {
                let date = rfc1123_now();
                for (name, value) in azure_shared_key_headers(url, account, k, &date) {
                    req = req.set(&name, &value);
                }
            }
            Backend::Azure { .. } => {}
            _ => {}
        }
        req.call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())
    }

    /// The Azure URL root; SAS tokens are appended by
    /// [`Self::azure_url`], not here.
    fn azure_root(&self) -> String {
        let Backend::Azure { account, endpoint, .. } = &self.backend else {
            unreachable!("azure_root on a non-Azure backend");
        };
        match endpoint {
            Some(e) => format!("{e}/{account}/{}", self.bucket),
            None => format!("https://{account}.blob.core.windows.net/{}", self.bucket),
        }
    }

    /// Append the SAS token (when one rides) to an Azure URL.
    fn azure_url(&self, base: String) -> String {
        let Backend::Azure { sas: Some(sas), .. } = &self.backend else {
            return base;
        };
        if base.contains('?') {
            format!("{base}&{sas}")
        } else {
            format!("{base}?{sas}")
        }
    }

    /// The S3 URL root: virtual-hosted on AWS, path-style behind
    /// an endpoint override.
    fn s3_root(&self) -> String {
        let Backend::S3 {
            region, endpoint, ..
        } = &self.backend
        else {
            unreachable!("s3_root on a non-S3 backend");
        };
        match endpoint {
            Some(e) => format!("{e}/{}", self.bucket),
            None if region == "us-east-1" => {
                format!("https://{}.s3.amazonaws.com", self.bucket)
            }
            None => format!("https://{}.s3.{region}.amazonaws.com", self.bucket),
        }
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
                Backend::S3 { .. } => {
                    let mut url = format!(
                        "{}/?list-type=2&delimiter=/&prefix={}",
                        self.s3_root(),
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
                Backend::Azure { .. } => {
                    let mut url = format!(
                        "{}?restype=container&comp=list&delimiter=/&prefix={}",
                        self.azure_root(),
                        urlencode(&dir)
                    );
                    if let Some(p) = &page {
                        url.push_str(&format!("&marker={}", urlencode(p)));
                    }
                    let xml = self.get(&self.azure_url(url))?;
                    let (ps, os, next) = parse_azure_listing(&xml)?;
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
            Backend::S3 { .. } => format!(
                "{}/{}",
                self.s3_root(),
                urlencode(&key).replace("%2F", "/")
            ),
            Backend::Azure { .. } => self.azure_url(format!(
                "{}/{}",
                self.azure_root(),
                urlencode(&key).replace("%2F", "/")
            )),
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

/// The current instant as an RFC 1123 date (`Fri, 24 Jul 2026
/// 18:00:00 GMT`) — what `x-ms-date` wants.
fn rfc1123_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs();
    let days = (secs / 86400) as i64;
    let (h, mi, sec) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        [((days + 4).rem_euclid(7)) as usize];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(mo - 1) as usize];
    format!("{weekday}, {d:02} {month} {y} {h:02}:{mi:02}:{sec:02} GMT")
}

/// The SharedKey authorization headers for a GET of `url` (2020
/// service version; empty body). Returns
/// (`x-ms-date`, `x-ms-version`, `authorization`) values paired
/// with their names.
fn azure_shared_key_headers(
    url: &str,
    account: &str,
    key: &[u8],
    date: &str,
) -> Vec<(String, String)> {
    let version = "2020-10-02";
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (_, path_query) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_query, ""),
    };
    // CanonicalizedResource: `/account` + the request path AS
    // SENT, then every query parameter as `\nname:value`, names
    // lowercased and sorted. On path-style (emulator) URLs the
    // path itself starts with the account, so the account
    // appears DOUBLED — verified against Azurite's own expected
    // string-to-sign; do not "fix" it by stripping.
    let decoded_path = String::from_utf8_lossy(&percent_decode_bytes(path)).into_owned();
    let mut params: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (
                k.to_lowercase(),
                String::from_utf8_lossy(&percent_decode_bytes(v)).into_owned(),
            ),
            None => (kv.to_lowercase(), String::new()),
        })
        .collect();
    params.sort();
    let mut resource = format!("/{account}/{decoded_path}");
    for (k, v) in &params {
        resource.push_str(&format!("\n{k}:{v}"));
    }
    let headers = format!("x-ms-date:{date}\nx-ms-version:{version}\n");
    let string_to_sign =
        format!("GET\n\n\n\n\n\n\n\n\n\n\n\n{headers}{resource}");
    let sig = quarb::base64(&hmac_sha256(key, string_to_sign.as_bytes()));
    vec![
        ("x-ms-date".to_string(), date.to_string()),
        ("x-ms-version".to_string(), version.to_string()),
        (
            "authorization".to_string(),
            format!("SharedKey {account}:{sig}"),
        ),
    ]
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&quarb::sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = quarb::sha256(&[ipad.as_slice(), msg].concat());
    quarb::sha256(&[opad.as_slice(), &inner].concat())
}

fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Parse one Azure `List Blobs` page: prefixes, blobs as
/// (key, size, last-modified), and the next marker.
fn parse_azure_listing(
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
    let mut objects: Vec<(String, Option<i64>, Option<String>)> = Vec::new();
    let mut next = None;
    let mut stack: Vec<String> = Vec::new();
    let mut cur_name = String::new();
    let mut cur_size = None;
    let mut cur_updated = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                stack.push(String::from_utf8_lossy(e.name().as_ref()).into_owned());
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "Blob" {
                    objects.push((
                        std::mem::take(&mut cur_name),
                        cur_size.take(),
                        cur_updated.take(),
                    ));
                }
                while stack.pop().is_some_and(|s| s != name) {}
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().map_err(|e| e.to_string())?.into_owned();
                match stack.as_slice() {
                    [.., a, b] if a == "BlobPrefix" && b == "Name" => {
                        prefixes.push(text.trim_end_matches('/').to_string());
                    }
                    [.., a, b] if a == "Blob" && b == "Name" => cur_name = text,
                    [.., a, b] if a == "Properties" && b == "Content-Length" => {
                        cur_size = text.parse().ok();
                    }
                    [.., a, b] if a == "Properties" && b == "Last-Modified" => {
                        cur_updated = Some(text);
                    }
                    [.., b] if b == "NextMarker" => {
                        if !text.is_empty() {
                            next = Some(text);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("listing XML: {e}")),
            _ => {}
        }
    }
    Ok((prefixes, objects, next))
}
