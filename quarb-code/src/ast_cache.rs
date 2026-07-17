//! Persistent per-file AST cache for [`CodeAdapter`](crate::CodeAdapter):
//! content-addressed, grammar-fingerprint-gated.
//!
//! On a cache HIT, [`load`] rebuilds the exact `Vec<Node>` a fresh
//! `build()` would produce and the caller skips tree-sitter; on a
//! MISS, the caller parses and [`store`] writes the arbor. The
//! design commitment: a hit either reconstructs a byte-identical
//! arbor or returns `None`. There is no "usually right" — any
//! header mismatch, footer mismatch, structural anomaly, short
//! read, or interning failure yields `None`, and the caller
//! reparses (correct, just not fast). So a corrupt, truncated, or
//! stale cache file can never produce a wrong answer.
//!
//! ## Format (little-endian; body varints are ULEB128)
//!
//! ```text
//! [124-byte header] [kind dict] [field dict] [N node records] [8-byte FNV footer]
//! ```
//!
//! The header carries a full grammar fingerprint — the tree-sitter
//! ABI version, the node-kind / field / parse-state counts, and a
//! SHA-256 digest of the entire id↔string tables — so any grammar
//! change that could alter parse output invalidates every entry
//! (the file lives under a digest-named directory too, so stale
//! entries are simply never looked up). The key is SHA-256 of the
//! source bytes; collision safety rests on those 256 bits, and the
//! FNV footer only guards against accidental corruption.
//!
//! The on-disk node record is seven ULEB128 varints, all
//! non-negative by construction (parent always precedes child;
//! pre-order start bytes and rows are monotone). `children` and
//! `fields` are NOT stored — they are rebuilt on load by replaying
//! `build()`'s exact linking step, which is why the reconstruction
//! is provably identical.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;

use quarb::NodeId;
use tree_sitter::Language;

use crate::Node;

const MAGIC: &[u8; 8] = b"QUARBAST";
/// Bump on any change to the on-disk layout — and also whenever a
/// release bumps a grammar dependency: builds without a reachable
/// Cargo.lock (`cargo install`) stamp the fingerprint "nolock", so
/// the version bump here is what re-keys their cache across a
/// scanner-only grammar upgrade the runtime tables cannot see.
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 124;

/// A cache rooted at a directory. Cheap to clone (a `PathBuf`).
#[derive(Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Cache { root }
    }

    /// `$QUARB_CACHE_DIR`, else `~/.quarb/cache`.
    pub fn default_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("QUARB_CACHE_DIR") {
            return PathBuf::from(d);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".quarb").join("cache")
    }
}

// ---------------------------------------------------------------------------
// Grammar fingerprint, memoized per language per process.
// ---------------------------------------------------------------------------

struct Meta {
    abi: u32,
    kind_count: u32,
    field_count: u32,
    state_count: u32,
    digest: [u8; 32],
}

thread_local! {
    static META: std::cell::RefCell<HashMap<u16, Rc<Meta>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The grammar fingerprint for `tag`/`lang` — hashing the full
/// node-kind and field id↔string tables so any renaming or
/// renumbering changes the digest. Memoized: computed once per
/// grammar per process, not per file.
fn meta(tag: u16, lang: &Language) -> Rc<Meta> {
    if let Some(m) = META.with(|m| m.borrow().get(&tag).cloned()) {
        return m;
    }
    let m = Rc::new(Meta {
        abi: lang.version() as u32,
        kind_count: lang.node_kind_count() as u32,
        field_count: lang.field_count() as u32,
        state_count: lang.parse_state_count() as u32,
        digest: grammar_digest(env!("QUARB_GRAMMAR_STAMP"), env!("CARGO_PKG_VERSION"), lang),
    });
    META.with(|mm| mm.borrow_mut().insert(tag, m.clone()));
    m
}

/// SHA-256 of the grammar fingerprint: the crate/grammar version
/// `stamp` and `crate_ver` (which cover the external scanner that
/// the runtime tables cannot), then the full node-kind and field
/// id↔string tables. Any renaming, renumbering, or grammar-crate
/// upgrade changes this digest, so a stale cache is never accepted.
fn grammar_digest(stamp: &str, crate_ver: &str, lang: &Language) -> [u8; 32] {
    let mut buf = Vec::new();
    for s in [stamp, crate_ver] {
        buf.extend((s.len() as u32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }
    let kc = lang.node_kind_count();
    buf.extend((kc as u32).to_le_bytes());
    for id in 0..kc as u16 {
        if let Some(s) = lang.node_kind_for_id(id) {
            buf.extend(id.to_le_bytes());
            buf.push(lang.node_kind_is_named(id) as u8);
            buf.extend((s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
    }
    let fc = lang.field_count();
    buf.extend((fc as u32).to_le_bytes());
    // Field ids are 1-based (NonZeroU16).
    for id in 1..=fc as u16 {
        if let Some(s) = lang.field_name_for_id(id) {
            buf.extend(id.to_le_bytes());
            buf.extend((s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
    }
    quarb::sha256(&buf)
}

/// The grammar tag for a file extension (mirrors [`crate::language`]).
/// 0 = unsupported.
pub(crate) fn lang_tag(ext: &str) -> u16 {
    match ext {
        "rs" => 1,
        "py" => 2,
        "js" | "mjs" | "cjs" | "jsx" => 3,
        "c" | "h" => 4,
        _ => 0,
    }
}

fn lang_name(tag: u16) -> &'static str {
    match tag {
        1 => "rust",
        2 => "python",
        3 => "javascript",
        4 => "c",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Primitives.
// ---------------------------------------------------------------------------

fn write_uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Read a ULEB128 from `buf` at `*pos`, advancing it. `None` on a
/// truncated or over-long (corrupt) encoding — never panics.
fn read_uleb(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        if shift >= 64 {
            return None; // more than 10 groups: corrupt
        }
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `<root>/ast/v<FMT>/<lang>-<digest8>/<hh>/<hex>.qast` — the format
/// generation, grammar-build, and 256-way content shard are all in
/// the path, so a version or grammar change can't even collide.
fn path_for(cache: &Cache, tag: u16, m: &Meta, content_hex: &str) -> PathBuf {
    let dig8: String = hex(&m.digest).chars().take(8).collect();
    cache
        .root
        .join("ast")
        .join(format!("v{FORMAT_VERSION}"))
        .join(format!("{}-{}", lang_name(tag), dig8))
        .join(&content_hex[..2])
        .join(format!("{content_hex}.qast"))
}

// ---------------------------------------------------------------------------
// Encode / decode.
// ---------------------------------------------------------------------------

/// Serialize `nodes` to the cache format. `None` if any node's kind
/// or field string fails to round-trip through the grammar's
/// id↔string tables (aliasing / unexpected symbol) — the caller
/// then simply does not cache this file.
fn encode(
    nodes: &[Node],
    tag: u16,
    lang: &Language,
    m: &Meta,
    content_hash: &[u8; 32],
    content_len: u64,
) -> Option<Vec<u8>> {
    let mut kind_ids: Vec<u16> = Vec::new();
    let mut kind_map: HashMap<u16, u32> = HashMap::new();
    let mut field_ids: Vec<u16> = Vec::new();
    let mut field_map: HashMap<u16, u32> = HashMap::new();
    let mut recs: Vec<(u32, u32)> = Vec::with_capacity(nodes.len());

    for n in nodes {
        // All interned nodes are named; the round-trip verify makes
        // the `named = true` assumption self-checking.
        let gid = lang.id_for_node_kind(n.kind, true);
        if lang.node_kind_for_id(gid) != Some(n.kind) {
            return None;
        }
        let kidx = *kind_map.entry(gid).or_insert_with(|| {
            let i = kind_ids.len() as u32;
            kind_ids.push(gid);
            i
        });
        let fidx1 = match n.field {
            None => 0u32,
            Some(f) => {
                let fid = lang.field_id_for_name(f)?.get();
                if lang.field_name_for_id(fid) != Some(f) {
                    return None;
                }
                let i = *field_map.entry(fid).or_insert_with(|| {
                    let i = field_ids.len() as u32;
                    field_ids.push(fid);
                    i
                });
                i + 1
            }
        };
        recs.push((kidx, fidx1));
    }

    let mut out = Vec::with_capacity(HEADER_LEN + nodes.len() * 8 + 8);
    out.extend_from_slice(MAGIC);
    out.extend(FORMAT_VERSION.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // flags
    out.extend(tag.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // pad
    out.extend(m.abi.to_le_bytes());
    out.extend(m.kind_count.to_le_bytes());
    out.extend(m.field_count.to_le_bytes());
    out.extend(m.state_count.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // reserved
    out.extend(0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&m.digest);
    out.extend_from_slice(content_hash);
    out.extend(content_len.to_le_bytes());
    out.extend((nodes.len() as u64).to_le_bytes());
    out.extend((kind_ids.len() as u16).to_le_bytes());
    out.extend((field_ids.len() as u16).to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_LEN);

    for &id in &kind_ids {
        write_uleb(&mut out, id as u64);
    }
    for &id in &field_ids {
        write_uleb(&mut out, id as u64);
    }

    let mut prev_start = 0u64;
    let mut prev_sline = 0u64;
    for (i, n) in nodes.iter().enumerate() {
        let (kidx, fidx1) = recs[i];
        let parent_delta = match n.parent {
            None => 0u64,
            Some(p) => (i as u64).checked_sub(p.0)?, // parent precedes child
        };
        let start = n.start as u64;
        let start_delta = start.checked_sub(prev_start)?;
        prev_start = start;
        let len = (n.end as u64).checked_sub(start)?;
        let sline = n.start_line as u64;
        let sline_delta = sline.checked_sub(prev_sline)?;
        prev_sline = sline;
        let espan = (n.end_line as u64).checked_sub(sline)?;

        write_uleb(&mut out, parent_delta);
        write_uleb(&mut out, kidx as u64);
        write_uleb(&mut out, fidx1 as u64);
        write_uleb(&mut out, start_delta);
        write_uleb(&mut out, len);
        write_uleb(&mut out, sline_delta);
        write_uleb(&mut out, espan);
    }

    let f = fnv1a(&out);
    out.extend(f.to_le_bytes());
    Some(out)
}

/// Deserialize, validating every invariant against `lang` and
/// `source`. `None` on any anomaly — the caller reparses.
fn decode(bytes: &[u8], tag: u16, lang: &Language, source: &str) -> Option<Vec<Node>> {
    if bytes.len() < HEADER_LEN + 8 {
        return None;
    }
    let body_len = bytes.len() - 8;
    let stored_footer = u64::from_le_bytes(bytes[body_len..].try_into().ok()?);
    if fnv1a(&bytes[..body_len]) != stored_footer {
        return None;
    }
    if &bytes[0..8] != MAGIC {
        return None;
    }
    let rd_u16 = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let rd_u32 = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let rd_u64 = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
    if rd_u16(8) != FORMAT_VERSION || rd_u16(12) != tag {
        return None;
    }
    let m = meta(tag, lang);
    if rd_u32(16) != m.abi
        || rd_u32(20) != m.kind_count
        || rd_u32(24) != m.field_count
        || rd_u32(28) != m.state_count
        || bytes[40..72] != m.digest
    {
        return None;
    }
    // The content is in hand (we read the file to key on it), so
    // verify the stored hash and length against it — a defence
    // against a wrong file landing at this path.
    if bytes[72..104] != quarb::sha256(source.as_bytes()) {
        return None;
    }
    if rd_u64(104) != source.len() as u64 {
        return None;
    }
    let node_count = rd_u64(112) as usize;
    let kdlen = rd_u16(120) as usize;
    let fdlen = rd_u16(122) as usize;

    let mut pos = HEADER_LEN;
    let mut kind_dict: Vec<&'static str> = Vec::with_capacity(kdlen);
    for _ in 0..kdlen {
        let gid = read_uleb(bytes, &mut pos)?;
        if gid > u16::MAX as u64 {
            return None;
        }
        kind_dict.push(lang.node_kind_for_id(gid as u16)?);
    }
    let mut field_dict: Vec<&'static str> = Vec::with_capacity(fdlen);
    for _ in 0..fdlen {
        let gid = read_uleb(bytes, &mut pos)?;
        if gid > u16::MAX as u64 {
            return None;
        }
        field_dict.push(lang.field_name_for_id(gid as u16)?);
    }

    let src_len = source.len() as u64;
    // node_count is attacker-controllable (a corrupt or poisoned
    // header). Each node is ≥7 varint bytes, so the real count cannot
    // exceed body_len/7; and each `Node` is ~128 bytes, so an
    // unbounded reservation would amplify a large body ~18×. Cap by
    // BOTH the logical bound and a modest absolute ceiling, so the
    // reservation is ≤ ~128 MB no matter what the header claims — a
    // lying count then just runs off the body and returns None below.
    // The Vec still grows normally for a genuinely huge (real) file.
    let reserve = node_count.min(body_len / 7).min(1 << 20);
    let mut nodes: Vec<Node> = Vec::with_capacity(reserve);
    let mut start = 0u64;
    let mut sline = 0u64;
    for i in 0..node_count {
        let parent_delta = read_uleb(bytes, &mut pos)?;
        let kidx = read_uleb(bytes, &mut pos)? as usize;
        let fidx1 = read_uleb(bytes, &mut pos)?;
        let start_delta = read_uleb(bytes, &mut pos)?;
        let len = read_uleb(bytes, &mut pos)?;
        let sline_delta = read_uleb(bytes, &mut pos)?;
        let espan = read_uleb(bytes, &mut pos)?;

        let parent = if i == 0 {
            if parent_delta != 0 {
                return None;
            }
            None
        } else {
            if parent_delta == 0 || parent_delta > i as u64 {
                return None;
            }
            Some(NodeId(i as u64 - parent_delta))
        };
        if kidx >= kdlen {
            return None;
        }
        let field = if fidx1 == 0 {
            None
        } else {
            let fi = (fidx1 - 1) as usize;
            if fi >= fdlen {
                return None;
            }
            Some(field_dict[fi])
        };
        start = start.checked_add(start_delta)?;
        let end = start.checked_add(len)?;
        if end > src_len {
            return None;
        }
        sline = sline.checked_add(sline_delta)?;
        let end_line = sline.checked_add(espan)?;

        nodes.push(Node {
            kind: kind_dict[kidx],
            field,
            parent,
            children: Vec::new(),
            start: start as usize,
            end: end as usize,
            start_line: sline as usize,
            end_line: end_line as usize,
            fields: Vec::new(),
        });
        // build()'s linking step, replayed exactly.
        if let Some(p) = parent {
            let pi = p.0 as usize;
            let clen = nodes[pi].children.len();
            if let Some(f) = field {
                nodes[pi].fields.push((f, clen));
            }
            nodes[pi].children.push(NodeId(i as u64));
        }
    }
    // Every byte between header and footer must be consumed exactly.
    if pos != body_len {
        return None;
    }
    Some(nodes)
}

// ---------------------------------------------------------------------------
// Public store / load (file I/O).
// ---------------------------------------------------------------------------

/// Try to load a cached arbor for `source`. `None` on a miss or any
/// validation failure. `tag` must be nonzero (a supported grammar).
pub(crate) fn load(
    cache: &Cache,
    tag: u16,
    lang: &Language,
    content_hash: &[u8; 32],
    source: &str,
) -> Option<Vec<Node>> {
    let m = meta(tag, lang);
    let path = path_for(cache, tag, &m, &hex(content_hash));
    let bytes = std::fs::read(&path).ok()?;
    decode(&bytes, tag, lang, source)
}

/// Best-effort store. Silent on any failure (interning refusal,
/// unwritable cache dir, races) — caching never breaks a query.
pub(crate) fn store(
    cache: &Cache,
    nodes: &[Node],
    tag: u16,
    lang: &Language,
    content_hash: &[u8; 32],
    content_len: u64,
) {
    let m = meta(tag, lang);
    let bytes = match encode(nodes, tag, lang, &m, content_hash, content_len) {
        Some(b) => b,
        None => return,
    };
    let path = path_for(cache, tag, &m, &hex(content_hash));
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Write to a temp file in the same directory, then atomically
    // rename into place. Concurrent writers produce byte-identical
    // images, so last-writer-wins is benign; a reader sees either
    // the old complete file or the new one, never a torn one.
    if let Ok(mut tmp) = tempfile::NamedTempFile::new_in(dir)
        && tmp.write_all(&bytes).is_ok()
    {
        let _ = tmp.persist(&path);
    }
}

#[cfg(test)]
mod tests;
