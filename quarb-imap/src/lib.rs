//! IMAP mailbox adapter for the Quarb query engine.
//!
//! Remote mailboxes, queried in place: folders as the root's
//! children, messages by UID, headers as properties
//! (`::subject`, `::from`, ...), the body as the value,
//! `::;epoch` for date predicates — the same mapping as
//! `quarb-maildir`, over the wire.
//!
//! The transport is `curl` (which speaks IMAP natively) over
//! subprocess — the same zero-new-dependencies posture as the
//! git adapter. Loading is lazy and per message: the folder list
//! is one `LIST`, a folder's UIDs one `UID SEARCH`, and each
//! touched message costs two fetches (headers, body) — so
//! peeking at one message of a ten-thousand-message inbox
//! fetches one message.
//!
//! **When to use it, honestly**: for a quick remote peek. Every
//! touched message is a network round-trip, and the thread
//! fabric (`~>` / `<-` over Message-IDs) is deliberately absent
//! — indexing it would mean fetching every header, which is a
//! sync. For real work, sync locally (mbsync) and use
//! `quarb-maildir`, where everything is instant and the fabric
//! works. This adapter exists so the remote option is real, and
//! measured.
//!
//! **Target**: `imap://HOST[:PORT][/FOLDER]` or `imaps://...`
//! (TLS). Credentials: `QUARB_IMAP_USER` / `QUARB_IMAP_PASS`,
//! else curl's `~/.netrc` (`machine HOST login U password P`).
//! Read-only: the adapter never stores, flags, or expunges.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to a mailbox.
#[derive(Debug, thiserror::Error)]
pub enum ImapError {
    #[error("imap: {0}")]
    Curl(String),
    #[error("imap target: {0} (expected imap[s]://HOST[:PORT][/FOLDER])")]
    Target(String),
}

enum Kind {
    Root,
    Folder {
        name: String,
    },
    /// A message: its folder and UID.
    Msg {
        folder: String,
        uid: u64,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A remote IMAP mailbox, exposed as an arbor.
pub struct ImapAdapter {
    /// `imap[s]://host[:port]` (no trailing slash).
    base: String,
    /// argv-safe auth flags: `-n` (netrc) or `-K -` (read the
    /// credential from stdin, keeping the password out of argv).
    auth: Vec<String>,
    /// curl `-K` config fed via stdin when credentials are set,
    /// so `user:password` never appears in the process's argv.
    auth_config: Option<String>,
    nodes: RefCell<Vec<Node>>,
    /// (folder, uid) → parsed headers (lowercased, unfolded).
    #[allow(clippy::type_complexity)]
    headers: RefCell<HashMap<(String, u64), Vec<(String, String)>>>,
    bodies: RefCell<HashMap<(String, u64), String>>,
}

fn parse_headers(raw: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(last) = out.last_mut()
        {
            last.1.push(' ');
            last.1.push_str(line.trim());
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    out
}

/// Quote a value for a curl `-K` config file's double-quoted
/// string, escaping the specials curl recognizes so the
/// credential round-trips byte-for-byte.
fn curl_config_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The mailbox name (final element) of a `LIST`/`LSUB` response
/// line, honoring the double-quoting IMAP uses so names with
/// spaces (e.g. `"Sent Items"`, `"[Gmail]/Sent Mail"`) survive
/// intact. Non-`LIST` lines yield `None`.
fn list_mailbox(line: &str) -> Option<String> {
    let line = line.trim_end();
    let rest = line
        .strip_prefix("* LIST")
        .or_else(|| line.strip_prefix("* LSUB"))?
        .trim_start();
    if !rest.ends_with('"') {
        // An atom name: the last whitespace-delimited token.
        return rest.split_whitespace().next_back().map(str::to_string);
    }
    // A quoted name: the last "..." on the line, unescaped.
    let mut start = None;
    let mut open = false;
    let mut esc = false;
    for (i, c) in rest.char_indices() {
        match (open, esc, c) {
            (false, _, '"') => {
                open = true;
                start = Some(i);
            }
            (true, false, '\\') => esc = true,
            (true, true, _) => esc = false,
            (true, false, '"') => open = false,
            _ => {}
        }
    }
    let start = start?;
    let raw = &rest[start + 1..rest.len() - 1];
    let mut out = String::with_capacity(raw.len());
    let mut esc = false;
    for c in raw.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Percent-encode a mailbox name for a curl IMAP URL path,
/// leaving the hierarchy separator `/` intact — curl decodes the
/// rest back to the raw name before issuing the IMAP command, so
/// spaces and other specials no longer break the URL.
fn encode_folder(folder: &str) -> String {
    let mut out = String::with_capacity(folder.len());
    for b in folder.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl ImapAdapter {
    /// Connect to `imap[s]://HOST[:PORT][/FOLDER]`. One `LIST`
    /// probes the connection (and scopes to FOLDER when given).
    pub fn connect(target: &str) -> Result<Self, ImapError> {
        if !target.starts_with("imap://") && !target.starts_with("imaps://") {
            return Err(ImapError::Target(target.to_string()));
        }
        let (scheme, rest) = target.split_once("://").unwrap();
        let (host, folder) = match rest.split_once('/') {
            Some((h, f)) if !f.is_empty() => (h, Some(f.to_string())),
            Some((h, _)) => (h, None),
            None => (rest, None),
        };
        if host.is_empty() {
            return Err(ImapError::Target(target.to_string()));
        }
        let (auth, auth_config) = match (
            std::env::var("QUARB_IMAP_USER"),
            std::env::var("QUARB_IMAP_PASS"),
        ) {
            // Feed the credential through a `-K -` config on stdin,
            // never an argv `--user` (readable via ps / /proc).
            (Ok(u), Ok(p)) => (
                vec!["-K".to_string(), "-".to_string()],
                Some(format!(
                    "user = {}\n",
                    curl_config_quote(&format!("{u}:{p}"))
                )),
            ),
            _ => (vec!["-n".to_string()], None),
        };
        let adapter = ImapAdapter {
            base: format!("{scheme}://{host}"),
            auth,
            auth_config,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            headers: RefCell::new(HashMap::new()),
            bodies: RefCell::new(HashMap::new()),
        };
        let folders = adapter.folders()?;
        // Scope to one folder when the target names it.
        let keep: Vec<String> = match folder {
            Some(f) => {
                if !folders.contains(&f) {
                    return Err(ImapError::Curl(format!("no such folder: {f}")));
                }
                vec![f]
            }
            None => folders,
        };
        let ids = keep
            .into_iter()
            .map(|f| adapter.push(Kind::Folder { name: f.clone() }, Some(f), Some(NodeId(0))))
            .collect();
        *adapter.nodes.borrow()[0].children.borrow_mut() = Some(ids);
        Ok(adapter)
    }

    /// A human-readable locator: `/folder/uid`.
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

    fn curl(&self, url: &str, command: Option<&str>) -> Result<String, ImapError> {
        use std::io::Write;
        use std::process::Stdio;
        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-s").arg("--url").arg(url).args(&self.auth);
        if let Some(c) = command {
            cmd.arg("-X").arg(c);
        }
        let out = if let Some(config) = &self.auth_config {
            // `-K -` reads the credential from stdin, so the
            // password never enters this curl's argv.
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn().map_err(|e| ImapError::Curl(e.to_string()))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(config.as_bytes())
                    .map_err(|e| ImapError::Curl(e.to_string()))?;
            }
            child
                .wait_with_output()
                .map_err(|e| ImapError::Curl(e.to_string()))?
        } else {
            cmd.output().map_err(|e| ImapError::Curl(e.to_string()))?
        };
        if !out.status.success() {
            return Err(ImapError::Curl(format!(
                "curl exit {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn folders(&self) -> Result<Vec<String>, ImapError> {
        let out = self.curl(&format!("{}/", self.base), Some(r#"LIST "" "*""#))?;
        Ok(out
            .lines()
            .filter_map(list_mailbox)
            .filter(|n| !n.is_empty())
            .collect())
    }

    fn push(&self, kind: Kind, name: Option<String>, parent: Option<NodeId>) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind,
            name,
            parent,
            children: RefCell::new(None),
        });
        id
    }

    /// A folder's message UIDs: one `UID SEARCH ALL`.
    fn uids(&self, folder: &str) -> Vec<u64> {
        let Ok(out) = self.curl(
            &format!("{}/{}", self.base, encode_folder(folder)),
            Some("UID SEARCH ALL"),
        ) else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|l| l.strip_prefix("* SEARCH"))
            .flat_map(|l| l.split_whitespace())
            .filter_map(|t| t.parse().ok())
            .collect()
    }

    /// A message's headers, fetched once.
    fn headers_of(&self, folder: &str, uid: u64) -> Vec<(String, String)> {
        let key = (folder.to_string(), uid);
        if let Some(h) = self.headers.borrow().get(&key) {
            return h.clone();
        }
        let raw = self
            .curl(
                &format!(
                    "{}/{};UID={uid};SECTION=HEADER",
                    self.base,
                    encode_folder(folder)
                ),
                None,
            )
            .unwrap_or_default();
        let h = parse_headers(&raw);
        self.headers.borrow_mut().insert(key, h.clone());
        h
    }

    fn body_of(&self, folder: &str, uid: u64) -> String {
        let key = (folder.to_string(), uid);
        if let Some(b) = self.bodies.borrow().get(&key) {
            return b.clone();
        }
        let raw = self
            .curl(
                &format!(
                    "{}/{};UID={uid};SECTION=TEXT",
                    self.base,
                    encode_folder(folder)
                ),
                None,
            )
            .unwrap_or_default()
            .replace("\r\n", "\n");
        self.bodies.borrow_mut().insert(key, raw.clone());
        raw
    }

    fn msg_ctx(&self, node: NodeId) -> Option<(String, u64)> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Msg { folder, uid } => Some((folder.clone(), *uid)),
            _ => None,
        }
    }
}

/// Reuse the maildir date parser's semantics for `::;epoch`.
fn parse_date(s: &str) -> Option<i64> {
    let s = s.split_once(',').map(|(_, r)| r).unwrap_or(s).trim();
    let mut it = s.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let mon_name = it.next()?;
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|&m| m == mon_name)? as i64
        + 1;
    let year: i64 = it.next()?.parse().ok()?;
    let mut t = it.next()?.split(':');
    let (h, m, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    let tz = it.next().unwrap_or("+0000");
    let off = match tz.split_at_checked(1) {
        Some(("+", v)) => {
            let v: i64 = v.parse().ok()?;
            (v / 100) * 3600 + (v % 100) * 60
        }
        Some(("-", v)) => {
            let v: i64 = v.parse().ok()?;
            -((v / 100) * 3600 + (v % 100) * 60)
        }
        _ => 0,
    };
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mon > 2 { mon - 3 } else { mon + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + m * 60 + sec - off)
}

impl AstAdapter for ImapAdapter {
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
        let folder = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Folder { name } => name.clone(),
            _ => return Vec::new(),
        };
        let ids: Vec<NodeId> = self
            .uids(&folder)
            .into_iter()
            .map(|uid| {
                self.push(
                    Kind::Msg {
                        folder: folder.clone(),
                        uid,
                    },
                    Some(uid.to_string()),
                    Some(node),
                )
            })
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

    /// `<folder>` / `<message>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Vec::new(),
            Kind::Folder { .. } => vec!["folder".to_string()],
            Kind::Msg { .. } => vec!["message".to_string()],
        }
    }

    /// Headers, lowercased (one fetch per message, cached).
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let (folder, uid) = self.msg_ctx(node)?;
        self.headers_of(&folder, uid)
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| Value::Str(v))
    }

    /// The message body (one fetch, cached).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (folder, uid) = self.msg_ctx(node)?;
        Some(Value::Str(self.body_of(&folder, uid)))
    }

    /// `::;epoch`, `::;uid`; `::;n-messages` on folders.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "n-messages" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Folder { .. } => Some(Value::Int(self.children(node).len() as i64)),
                _ => None,
            },
            "uid" => self.msg_ctx(node).map(|(_, u)| Value::Int(u as i64)),
            "epoch" => {
                let (folder, uid) = self.msg_ctx(node)?;
                let hs = self.headers_of(&folder, uid);
                let date = hs.iter().find(|(k, _)| k == "date")?;
                parse_date(&date.1).map(Value::Int)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_mailbox_quoted_name_with_space() {
        // Regression: the whole quoted name survives, not just
        // the last space-separated token.
        assert_eq!(
            list_mailbox(r#"* LIST (\HasNoChildren) "/" "Sent Items""#),
            Some("Sent Items".to_string())
        );
    }

    #[test]
    fn list_mailbox_atom_name() {
        assert_eq!(
            list_mailbox(r#"* LIST (\HasNoChildren) "/" INBOX"#),
            Some("INBOX".to_string())
        );
    }

    #[test]
    fn list_mailbox_gmail_hierarchy_with_space() {
        assert_eq!(
            list_mailbox(r#"* LIST (\HasNoChildren) "/" "[Gmail]/Sent Mail""#),
            Some("[Gmail]/Sent Mail".to_string())
        );
    }

    #[test]
    fn list_mailbox_nil_delimiter() {
        assert_eq!(
            list_mailbox(r#"* LIST (\Noselect) NIL "Top Level""#),
            Some("Top Level".to_string())
        );
    }

    #[test]
    fn list_mailbox_unescapes_embedded_quote() {
        assert_eq!(
            list_mailbox(r#"* LIST () "/" "It's \"odd\"""#),
            Some("It's \"odd\"".to_string())
        );
    }

    #[test]
    fn list_mailbox_ignores_non_list_lines() {
        assert_eq!(list_mailbox("A001 OK LIST completed"), None);
    }

    #[test]
    fn encode_folder_encodes_space_keeps_slash() {
        // Regression: spaces become %20 (curl no longer rejects
        // the URL); the hierarchy separator stays literal.
        assert_eq!(
            encode_folder("[Gmail]/Sent Mail"),
            "%5BGmail%5D/Sent%20Mail"
        );
        // Simple names are byte-identical to the old interpolation.
        assert_eq!(encode_folder("INBOX"), "INBOX");
    }

    #[test]
    fn curl_config_quote_escapes_backslash_and_quote() {
        assert_eq!(curl_config_quote("u:p"), r#""u:p""#);
        assert_eq!(curl_config_quote(r#"u:pa"ss\x"#), r#""u:pa\"ss\\x""#);
    }
}
