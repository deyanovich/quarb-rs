//! CBOR adapter for Quarb.
//!
//! Maps a CBOR document (RFC 8949) onto the arbor model the way
//! the JSON adapter does, honoring what CBOR adds beyond JSON:
//!
//! - Maps, arrays, and primitives are nodes; a node's *name* is
//!   its map key or array index. CBOR map keys need not be text —
//!   integer and other scalar keys name their nodes by their
//!   canonical rendering (`42`, `true`), so `/42/name` addresses
//!   an integer-keyed entry.
//! - Traits carry the CBOR type: `<map>`, `<array>`, `<string>`,
//!   `<number>`, `<boolean>`, `<bytes>`, `<null>`, `<instant>`.
//! - Byte strings are scalars whose value is the lowercase hex
//!   rendering (Quarb's `hex` built-in speaks the same), with
//!   `;;;length` answering the raw byte count.
//! - Standard time tags decode to *instants*: tag 0 (RFC 3339
//!   text) and tag 1 (epoch seconds, integer or float), so
//!   `[::when > 2026-01-01]` is a calendar comparison. Other
//!   tags are transparent — the node is its content — with the
//!   tag number answering as `;;;tag`.
//! - `;;;type` and `;;;length` as in the JSON adapter.
//!
//! Map entries keep document order. Non-finite floats and
//! bignums render as strings rather than lying about precision.

use ciborium::value::Value as Cbor;
use quarb::{AstAdapter, NodeId, Value};

/// An error reading a CBOR document.
#[derive(Debug, thiserror::Error)]
pub enum CborError {
    #[error("cbor: {0}")]
    Decode(String),
}

/// The CBOR type of a node, used as its trait.
#[derive(Clone, Copy)]
enum Kind {
    Map,
    Array,
    String,
    Number,
    Boolean,
    Bytes,
    Null,
    Instant,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Map => "map",
            Kind::Array => "array",
            Kind::String => "string",
            Kind::Number => "number",
            Kind::Boolean => "boolean",
            Kind::Bytes => "bytes",
            Kind::Null => "null",
            Kind::Instant => "instant",
        }
    }
}

struct Node {
    name: Option<String>,
    kind: Kind,
    /// The scalar value for a primitive; `Value::Null` for
    /// containers.
    scalar: Value,
    /// Raw byte count for byte strings (`;;;length`).
    bytes_len: Option<usize>,
    /// The enclosing tag number, when one wrapped this node.
    tag: Option<u64>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A Quarb adapter over a parsed CBOR document.
pub struct CborAdapter {
    nodes: Vec<Node>,
    root: NodeId,
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The display name for a map key. Text keys name directly;
/// other scalar keys use their canonical rendering.
fn key_name(k: &Cbor) -> String {
    match k {
        Cbor::Text(s) => s.clone(),
        Cbor::Integer(i) => i128::from(*i).to_string(),
        Cbor::Bool(b) => b.to_string(),
        Cbor::Float(f) => f.to_string(),
        Cbor::Bytes(b) => hex(b),
        Cbor::Null => "null".to_string(),
        _ => "?".to_string(),
    }
}

/// Decode a standard time tag's content to an instant.
fn time_instant(tag: u64, inner: &Cbor) -> Option<Value> {
    match (tag, inner) {
        (0, Cbor::Text(s)) => {
            let (secs, nanos, offset_min) = quarb::temporal::parse_iso(s)?;
            Some(Value::Instant {
                secs,
                nanos,
                offset_min,
            })
        }
        (1, Cbor::Integer(i)) => Some(Value::Instant {
            secs: i128::from(*i) as i64,
            nanos: 0,
            offset_min: None,
        }),
        (1, Cbor::Float(f)) => Some(Value::Instant {
            secs: f.floor() as i64,
            nanos: ((f - f.floor()) * 1e9) as u32,
            offset_min: None,
        }),
        _ => None,
    }
}

fn build(
    value: &Cbor,
    name: Option<String>,
    parent: Option<NodeId>,
    tag: Option<u64>,
    nodes: &mut Vec<Node>,
) -> NodeId {
    // A tag wraps its content: time tags become instant scalars,
    // any other tag passes through to its content node, which
    // remembers the number for `;;;tag`.
    if let Cbor::Tag(t, inner) = value {
        if let Some(v) = time_instant(*t, inner) {
            let id = NodeId(nodes.len() as u64);
            nodes.push(Node {
                name,
                kind: Kind::Instant,
                scalar: v,
                bytes_len: None,
                tag: Some(*t),
                parent,
                children: Vec::new(),
            });
            return id;
        }
        return build(inner, name, parent, Some(*t), nodes);
    }

    let (kind, scalar, bytes_len) = match value {
        Cbor::Map(_) => (Kind::Map, Value::Null, None),
        Cbor::Array(_) => (Kind::Array, Value::Null, None),
        Cbor::Text(s) => (Kind::String, Value::Str(s.clone()), None),
        Cbor::Integer(i) => {
            let i = i128::from(*i);
            let v = i64::try_from(i)
                .map(Value::Int)
                .unwrap_or_else(|_| Value::Str(i.to_string()));
            (Kind::Number, v, None)
        }
        Cbor::Float(f) => (Kind::Number, Value::Float(*f), None),
        Cbor::Bool(b) => (Kind::Boolean, Value::Bool(*b), None),
        Cbor::Bytes(b) => (Kind::Bytes, Value::Str(hex(b)), Some(b.len())),
        Cbor::Null => (Kind::Null, Value::Null, None),
        _ => (Kind::Null, Value::Null, None),
    };

    let id = NodeId(nodes.len() as u64);
    nodes.push(Node {
        name,
        kind,
        scalar,
        bytes_len,
        tag,
        parent,
        children: Vec::new(),
    });

    match value {
        Cbor::Map(entries) => {
            for (k, v) in entries {
                let c = build(v, Some(key_name(k)), Some(id), None, nodes);
                nodes[id.0 as usize].children.push(c);
            }
        }
        Cbor::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                let c = build(v, Some(i.to_string()), Some(id), None, nodes);
                nodes[id.0 as usize].children.push(c);
            }
        }
        _ => {}
    }
    id
}

impl CborAdapter {
    /// Decode `bytes` as a single CBOR item and build the
    /// adapter.
    pub fn parse(bytes: &[u8]) -> Result<Self, CborError> {
        let value: Cbor =
            ciborium::from_reader(bytes).map_err(|e| CborError::Decode(e.to_string()))?;
        let mut nodes = Vec::new();
        build(&value, None, None, None, &mut nodes);
        Ok(CborAdapter {
            nodes,
            root: NodeId(0),
        })
    }

    /// A pointer-style path to `node` (`/users/0/name`), for
    /// rendering.
    pub fn pointer(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(name) = &n.name {
                parts.push(name.clone());
            }
            cur = n.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }
}

impl AstAdapter for CborAdapter {
    fn root(&self) -> NodeId {
        self.root
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        vec![self.nodes[node.0 as usize].kind.name().to_string()]
    }

    /// A map's scalar entries answer as properties, like the
    /// JSON adapter's objects.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        for &c in &n.children {
            let child = &self.nodes[c.0 as usize];
            if child.name.as_deref() == Some(name) {
                return match child.scalar {
                    Value::Null => None,
                    ref v => Some(v.clone()),
                };
            }
        }
        None
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes[node.0 as usize].scalar {
            Value::Null => None,
            v => Some(v.clone()),
        }
    }

    /// `;;;type`, `;;;length` (entries for containers, raw byte
    /// count for byte strings, characters for text), and
    /// `;;;tag` (the enclosing CBOR tag number, when any).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "type" => Some(Value::Str(n.kind.name().to_string())),
            "length" => match n.kind {
                Kind::Map | Kind::Array => Some(Value::Int(n.children.len() as i64)),
                Kind::Bytes => Some(Value::Int(n.bytes_len? as i64)),
                Kind::String => match &n.scalar {
                    Value::Str(s) => Some(Value::Int(s.chars().count() as i64)),
                    _ => None,
                },
                _ => None,
            },
            "tag" => n.tag.map(|t| Value::Int(t as i64)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(v: &Cbor) -> Vec<u8> {
        let mut out = Vec::new();
        ciborium::into_writer(v, &mut out).unwrap();
        out
    }

    fn music() -> Vec<u8> {
        // {"tracks": [{"title": "Mars", "price": 1.29,
        //              "added": 1(1700000000), "art": h'c0ffee',
        //              42: "int-keyed"}]}
        let track = Cbor::Map(vec![
            (Cbor::Text("title".into()), Cbor::Text("Mars".into())),
            (Cbor::Text("price".into()), Cbor::Float(1.29)),
            (
                Cbor::Text("added".into()),
                Cbor::Tag(1, Box::new(Cbor::Integer(1_700_000_000.into()))),
            ),
            (
                Cbor::Text("art".into()),
                Cbor::Bytes(vec![0xc0, 0xff, 0xee]),
            ),
            (Cbor::Integer(42.into()), Cbor::Text("int-keyed".into())),
        ]);
        encode(&Cbor::Map(vec![(
            Cbor::Text("tracks".into()),
            Cbor::Array(vec![track]),
        )]))
    }

    fn v(a: &CborAdapter, q: &str) -> Vec<String> {
        match quarb::run(q, a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.pointer(n)).collect(),
        }
    }

    #[test]
    fn navigates_like_json() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/title::"), ["Mars"]);
        assert_eq!(v(&a, "/tracks/0::price"), ["1.29"]);
        assert_eq!(v(&a, "/tracks;;;length"), ["1"]);
    }

    #[test]
    fn time_tags_become_instants() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/added;;;type"), ["instant"]);
        assert_eq!(v(&a, "/tracks/*[::added > 2023-01-01] @| count"), ["1"]);
        assert_eq!(v(&a, "/tracks/0/added;;;tag"), ["1"]);
    }

    #[test]
    fn bytes_render_hex_with_raw_length() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/art::"), ["c0ffee"]);
        assert_eq!(v(&a, "/tracks/0/art;;;length"), ["3"]);
        assert_eq!(v(&a, "/tracks/0/*<bytes> @| count"), ["1"]);
    }

    #[test]
    fn nontext_keys_address_by_rendering() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/42::"), ["int-keyed"]);
    }

    #[test]
    fn rfc3339_tag_zero() {
        let doc = encode(&Cbor::Map(vec![(
            Cbor::Text("when".into()),
            Cbor::Tag(0, Box::new(Cbor::Text("2026-07-20T12:00:00Z".into()))),
        )]));
        let a = CborAdapter::parse(&doc).unwrap();
        assert_eq!(v(&a, "/when;;;type"), ["instant"]);
    }
}
