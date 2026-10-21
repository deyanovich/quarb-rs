//! Maildir and mbox adapter for the Quarb query engine.
//!
//! A mailbox is a flat store whose *real* structure is the thread
//! graph, and the thread graph is reference fabric: every message
//! carries a `Message-ID`, replies carry `In-Reply-To`, and the
//! adapter indexes the ids so that `::in-reply-to-->` walks to the
//! parent message and `<-in-reply-to` finds the replies — the same
//! two axes that walk commit parents and foreign keys.
//!
//! - **Maildir**: a directory with `cur/` and `new/`; every file
//!   is a message, named by its filename's unique prefix (up to
//!   the first `:`).
//! - **mbox**: a single file of `From `-separated messages, named
//!   by ordinal.
//!
//! Messages expose their headers as properties, lowercased
//! (`::subject`, `::from`, `::to`, `::date`, `::in-reply-to`,
//! `::message-id`, ...) with RFC 2047 left as-is (v1); the body
//! is the node's value (`::`). For multipart messages the body is
//! the raw MIME body — as stored, not pretty; a `text/plain` part
//! extractor is a recorded refinement. `:::n-messages` on the
//! root; `<message>` trait; `:::epoch` parses the `Date` header
//! to a unix timestamp for range predicates.
//!
//! Read-only, entirely: the adapter never touches flags, never
//! moves files between `new/` and `cur/`.

use quarb::source::SourceDecoding;
use quarb::{AstAdapter, NodeId, Value};
use std::collections::HashMap;
use std::path::Path;

/// An error opening a mailbox.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("maildir: {0}")]
    Io(#[from] std::io::Error),
    #[error("maildir: {0} is neither a Maildir (cur/ + new/) nor an mbox file")]
    Format(String),
    #[error("maildir: {0}: {1}")]
    Source(String, quarb::source::SourceError),
}

/// The charset a message declares in its own headers
/// (`Content-Type: text/plain; charset=koi8-r`), reported for the
/// engine's source boundary; the headers themselves are ASCII by
/// RFC 5322, so the scan reads bytes. The adapter decodes nothing.
pub fn declared_encoding(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| bytes.windows(4).position(|w| w == b"\r\n\r\n"))
        .unwrap_or(bytes.len());
    let head = String::from_utf8_lossy(&bytes[..end]);
    let mut unfolded = String::new();
    for line in head.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            unfolded.push(' ');
            unfolded.push_str(line.trim());
        } else {
            unfolded.push('\n');
            unfolded.push_str(line);
        }
    }
    let line = unfolded
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-type:"))?;
    let at = line.to_ascii_lowercase().find("charset=")?;
    let label = line[at + 8..]
        .trim()
        .trim_matches('"')
        .split([';', ' ', '"'])
        .next()?
        .trim();
    (!label.is_empty()).then(|| label.to_string())
}

struct Message {
    name: String,
    /// Lowercased header name → unfolded value.
    headers: Vec<(String, String)>,
    body: String,
    /// The backing file (Maildir; mbox messages have none) — the
    /// stable handle tools act on (flagging, moving); the adapter
    /// itself never writes.
    path: Option<std::path::PathBuf>,
}

/// A mailbox, exposed as an arbor of messages.
pub struct MaildirAdapter {
    messages: Vec<Message>,
    /// Message-ID (angle brackets stripped) → message index.
    by_id: HashMap<String, usize>,
    /// Per message: the decoding record from the source boundary
    /// (`:::encoding`, `:::replaced`).
    records: Vec<(String, usize)>,
}

/// Parse one RFC 822 message: headers (unfolded) and body.
fn parse_message(raw: &str, name: String, path: Option<std::path::PathBuf>) -> Message {
    // Headers end at the first blank line. Accept both LF
    // (`\n\n`) and CRLF (`\r\n\r\n`) separators — a CRLF message
    // (as delivered by many MTAs) would otherwise parse as all
    // headers and an empty body, and body lines with colons would
    // become bogus headers.
    let sep = [
        raw.find("\r\n\r\n").map(|i| (i, i + 4)),
        raw.find("\n\n").map(|i| (i, i + 2)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|&(start, _)| start);
    let (head, body) = match sep {
        Some((start, end)) => (&raw[..start], raw[end..].to_string()),
        None => (raw, String::new()),
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(last) = headers.last_mut()
        {
            last.1.push(' ');
            last.1.push_str(line.trim());
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    // Encoded words are decoded once the header is unfolded: a word
    // may continue on the next line, and whitespace between two
    // words is theirs to drop.
    for h in &mut headers {
        h.1 = decode_words(&h.1);
    }
    Message {
        name,
        headers,
        body,
        path,
    }
}

/// RFC 2047 encoded words in a header value
/// (`=?windows-1251?B?...?=`, `=?utf-8?Q?...?=`): decoded under
/// their own charset through the source boundary's tables, the
/// text between them kept, whitespace between two adjacent words
/// dropped as the RFC says. A word that does not decode stays as
/// written.
fn decode_words(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_string();
    }
    let mut out = String::new();
    let mut rest = value;
    let mut after_word = false;
    while let Some(start) = rest.find("=?") {
        let before = &rest[..start];
        if !(after_word && before.trim().is_empty()) {
            out.push_str(before);
        }
        let Some((word, tail)) = encoded_word(&rest[start..]) else {
            out.push_str("=?");
            rest = &rest[start + 2..];
            after_word = false;
            continue;
        };
        out.push_str(&word);
        rest = tail;
        after_word = true;
    }
    out.push_str(rest);
    out
}

/// One `=?charset?enc?text?=` at the start of `s`: the decoded text
/// and what follows it.
fn encoded_word(s: &str) -> Option<(String, &str)> {
    // `=?charset?enc?text?=`: the terminator is looked for after
    // the third `?`, since Q text may itself begin with `=XX`.
    let body = s.strip_prefix("=?")?;
    let (charset, rest) = body.split_once('?')?;
    let charset = charset.split('*').next().unwrap_or(charset);
    let (enc, rest) = rest.split_once('?')?;
    let end = rest.find("?=")?;
    let text = &rest[..end];
    let tail = &rest[end + 2..];
    let bytes = match enc {
        "B" | "b" => quarb::base64_decode(text)?,
        "Q" | "q" => {
            let b = text.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'_' => out.push(b' '),
                    b'=' if i + 2 < b.len() => {
                        let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
                        out.push(u8::from_str_radix(hex, 16).ok()?);
                        i += 2;
                    }
                    c => out.push(c),
                }
                i += 1;
            }
            out
        }
        _ => return None,
    };
    let decoded = quarb::source::decode_label(&bytes, charset)?;
    Some((decoded, tail))
}

/// A `Date:` header value to a unix timestamp (RFC 2822-ish;
/// naive parser, UTC offsets honored).
fn parse_date(s: &str) -> Option<i64> {
    parse_date_full(s).map(|(secs, _)| secs)
}

/// [`parse_date`] keeping the written UTC offset (minutes), for
/// the minted `:::date` instant.
fn parse_date_full(s: &str) -> Option<(i64, i16)> {
    // "Tue, 7 Jul 2026 12:00:00 +0200" — day-of-week optional.
    let s = s.split_once(',').map(|(_, r)| r).unwrap_or(s).trim();
    let mut it = s.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let mon = match it.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = it.next()?.parse().ok()?;
    // A year outside the four-digit range is no date this header
    // can carry (and would overflow the day arithmetic below).
    if !(0..=9999).contains(&year) || !(1..=31).contains(&day) {
        return None;
    }
    let hms = it.next()?;
    let mut t = hms.split(':');
    let (h, m, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    let tz = it.next().unwrap_or("+0000");
    let off = if let Some(rest) = tz.strip_prefix('+') {
        let v: i64 = rest.parse().ok()?;
        (v / 100) * 3600 + (v % 100) * 60
    } else if let Some(rest) = tz.strip_prefix('-') {
        let v: i64 = rest.parse().ok()?;
        -((v / 100) * 3600 + (v % 100) * 60)
    } else {
        0
    };
    // Days since epoch (civil-from-days inverse, Howard Hinnant's
    // algorithm).
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mon > 2 { mon - 3 } else { mon + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((
        days * 86400 + h * 3600 + m * 60 + sec - off,
        (off / 60) as i16,
    ))
}

impl MaildirAdapter {
    /// Open a Maildir directory or an mbox file.
    pub fn open(path: &Path) -> Result<Self, MailError> {
        Self::open_with_decoding(
            path,
            SourceDecoding::default().with_declared(|_, b| declared_encoding(b)),
        )
    }

    /// [`open`](Self::open) under a mount's source-boundary rule:
    /// every message's bytes decode by `?encoding=`, then a
    /// byte-order mark, then the charset its own headers declare,
    /// then strict UTF-8 — a message that decodes under none is
    /// refused, naming the option.
    pub fn open_with_decoding(path: &Path, decoding: SourceDecoding) -> Result<Self, MailError> {
        let mut messages = Vec::new();
        let mut records = Vec::new();
        let decode = |name: &str, bytes: &[u8]| {
            decoding
                .decode(name, bytes)
                .map_err(|e| MailError::Source(name.to_string(), e))
        };
        if path.is_dir() {
            let cur = path.join("cur");
            let new = path.join("new");
            if !cur.is_dir() && !new.is_dir() {
                return Err(MailError::Format(path.display().to_string()));
            }
            for dir in [cur, new] {
                if !dir.is_dir() {
                    continue;
                }
                let mut files: Vec<_> = std::fs::read_dir(&dir)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect();
                files.sort();
                for f in files {
                    let bytes = std::fs::read(&f).unwrap_or_default();
                    let base = f.file_name().unwrap_or_default().to_string_lossy();
                    // The unique portion, before the flags part.
                    let name = base.split(':').next().unwrap_or(&base).to_string();
                    let d = decode(&f.display().to_string(), &bytes)?;
                    records.push((d.encoding, d.replaced));
                    messages.push(parse_message(&d.text, name, Some(f.clone())));
                }
            }
        } else {
            let bytes = std::fs::read(path)?;
            if !bytes.starts_with(b"From ") {
                return Err(MailError::Format(path.display().to_string()));
            }
            // Message by message: each declares its own charset.
            let mut starts = vec![0];
            starts.extend(
                bytes
                    .windows(6)
                    .enumerate()
                    .filter(|(_, w)| *w == b"\nFrom ")
                    .map(|(i, _)| i + 1),
            );
            starts.push(bytes.len());
            for (i, pair) in starts.windows(2).enumerate() {
                let chunk = &bytes[pair[0]..pair[1]];
                let chunk = chunk.strip_prefix(b"From ").unwrap_or(chunk);
                // Drop the "From " envelope line itself.
                let body = chunk
                    .iter()
                    .position(|b| *b == b'\n')
                    .map(|i| &chunk[i + 1..])
                    .unwrap_or(&[]);
                let body = body.strip_suffix(b"\n").unwrap_or(body);
                let d = decode(&format!("{}:{}", path.display(), i + 1), body)?;
                records.push((d.encoding, d.replaced));
                messages.push(parse_message(&d.text, (i + 1).to_string(), None));
            }
        }
        let by_id = messages
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                m.headers
                    .iter()
                    .find(|(k, _)| k == "message-id")
                    .map(|(_, v)| (v.trim_matches(['<', '>']).to_string(), i))
            })
            .collect();
        Ok(MaildirAdapter {
            messages,
            by_id,
            records,
        })
    }

    /// A human-readable locator: `/name`.
    pub fn locator(&self, node: NodeId) -> String {
        match node.0 {
            0 => "/".to_string(),
            n => format!("/{}", self.messages[n as usize - 1].name),
        }
    }

    /// A message's backing file — the stable handle for
    /// tool-owned actions (Maildir flag renames). `None` for mbox
    /// messages and the root.
    pub fn path_of(&self, node: NodeId) -> Option<&Path> {
        self.msg(node)?.path.as_deref()
    }

    fn msg(&self, node: NodeId) -> Option<&Message> {
        self.messages.get((node.0 as usize).checked_sub(1)?)
    }

    fn header(&self, node: NodeId, name: &str) -> Option<&str> {
        self.msg(node)?
            .headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

impl AstAdapter for MaildirAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 != 0 {
            return Vec::new();
        }
        (1..=self.messages.len() as u64).map(NodeId).collect()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        Some(self.msg(node)?.name.clone())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        (node.0 != 0).then_some(NodeId(0))
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        if node.0 == 0 {
            return Vec::new();
        }
        vec!["message".to_string()]
    }

    /// Headers, lowercased: `::subject`, `::from`,
    /// `::in-reply-to`, ...
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.header(node, name).map(|v| Value::Str(v.to_string()))
    }

    /// The message body.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(self.msg(node)?.body.clone()))
    }

    /// `:::epoch` (the Date header as a unix timestamp),
    /// `:::date` (the same, minted as an instant),
    /// `:::n-headers`; `:::n-messages` on the root.
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["n-messages", "encoding", "replaced", "new", "epoch", "date", "n-headers"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            return match key {
                "n-messages" => Some(Value::Int(self.messages.len() as i64)),
                _ => None,
            };
        }
        match key {
            "encoding" => Some(Value::Str(
                self.records
                    .get((node.0 as usize).checked_sub(1)?)?
                    .0
                    .clone(),
            )),
            "replaced" => Some(Value::Int(
                self.records.get((node.0 as usize).checked_sub(1)?)?.1 as i64,
            )),
            // Whether the message still sits in new/ (Maildir's
            // "unseen" signal) — `/*[:::new]` is the unread query.
            "new" => Some(Value::Bool(
                self.msg(node)?
                    .path
                    .as_deref()
                    .and_then(|p| p.parent())
                    .and_then(|d| d.file_name())
                    .is_some_and(|d| d == "new"),
            )),
            "epoch" => parse_date(self.header(node, "date")?).map(Value::Int),
            // The Date header, minted as an instant (the written
            // offset preserved for display).
            "date" => parse_date_full(self.header(node, "date")?).map(|(secs, offset_min)| {
                Value::Instant {
                    secs,
                    nanos: 0,
                    offset_min: Some(offset_min),
                    calendar: Default::default(),
                }
            }),
            "n-headers" => Some(Value::Int(self.msg(node)?.headers.len() as i64)),
            _ => None,
        }
    }

    /// `::in-reply-to-->` (or any header holding a message id)
    /// resolves through the Message-ID index.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let v = self.header(node, property)?;
        // In-Reply-To / References may hold several ids; the first
        // resolvable one wins.
        v.split_whitespace()
            .filter_map(|id| self.by_id.get(id.trim_matches(['<', '>'])))
            .map(|&i| NodeId(i as u64 + 1))
            .next()
    }

    /// The thread fabric: an `in-reply-to` edge to the parent
    /// message when it is present in this mailbox.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.resolve(node, "in-reply-to", None)
            .map(|t| ("in-reply-to".to_string(), t))
            .into_iter()
            .collect()
    }

    /// A message's reference is what it replies to: the bare arrows
    /// read `in-reply-to`.
    fn ref_property(&self, node: NodeId) -> Option<String> {
        self.header(node, "in-reply-to")
            .map(|_| "in-reply-to".to_string())
    }

    /// `<--(::in-reply-to)` is the replies, with no walk; any other
    /// header keeps the engine's search.
    fn reverse_resolve(
        &self,
        node: NodeId,
        property: Option<&str>,
        _hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        // With no property named: the message's reference property.
        if property.is_some_and(|p| p != "in-reply-to") {
            return None;
        }
        self.header(node, "message-id")?;
        Some(self.backlinks(node).into_iter().map(|(_, n)| n).collect())
    }

    /// Replies: the messages whose `in-reply-to` points here.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some(my_id) = self.header(node, "message-id") else {
            return Vec::new();
        };
        let my_id = my_id.trim_matches(['<', '>']).to_string();
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.headers.iter().any(|(k, v)| {
                    k == "in-reply-to"
                        && v.split_whitespace()
                            .any(|i| i.trim_matches(['<', '>']) == my_id)
                })
            })
            .map(|(i, _)| ("in-reply-to".to_string(), NodeId(i as u64 + 1)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 19: a year outside four digits is no date (and
    /// would overflow the day arithmetic).
    #[test]
    fn absurd_dates_are_none() {
        assert_eq!(
            parse_date("Mon, 1 Jan 99999999999999999 00:00:00 +0000"),
            None
        );
        assert_eq!(parse_date("Mon, 99 Jan 2024 00:00:00 +0000"), None);
        assert_eq!(parse_date("Thu, 1 Jan 1970 00:00:00 +0000"), Some(0));
    }

    #[test]
    fn crlf_message_splits_headers_from_body() {
        // A CRLF-delimited message: the header/body boundary is
        // `\r\n\r\n`, not `\n\n`.
        let raw = "Subject: hi\r\nFrom: a@b\r\n\r\nPS: see below\r\nin-reply-to: !a header\r\n";
        let m = parse_message(raw, "n".to_string(), None);
        // Exactly the two real headers, not the body lines misread
        // as headers.
        assert_eq!(m.headers.len(), 2);
        assert_eq!(
            m.headers
                .iter()
                .find(|(k, _)| k == "subject")
                .map(|(_, v)| v.as_str()),
            Some("hi")
        );
        assert!(m.body.starts_with("PS: see below"));
        // The body's "in-reply-to: ..." line must not inject a
        // bogus thread edge.
        assert!(m.headers.iter().all(|(k, _)| k != "in-reply-to"));
    }

    #[test]
    fn lf_message_splits_unchanged() {
        // The existing LF path is preserved exactly.
        let raw = "Subject: hi\n\nbody here\n";
        let m = parse_message(raw, "n".to_string(), None);
        assert_eq!(m.headers.len(), 1);
        assert_eq!(m.body, "body here\n");
    }
}
