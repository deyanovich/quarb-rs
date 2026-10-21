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
//!   `:::size` answering the raw byte count as a size in bytes.
//! - Standard time tags decode to *instants*: tag 0 (RFC 3339
//!   text) and tag 1 (epoch seconds, integer or float), so
//!   `[::when > 2026-01-01]` is a calendar comparison. Other
//!   tags are transparent — the node is its content — with the
//!   tag number answering as `:::tag`.
//! - `:::type` and `:::length` (a count) as in the JSON adapter.
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
    /// Raw byte count for byte strings (`:::size`, a quantity).
    bytes_len: Option<usize>,
    /// The enclosing tag number, when one wrapped this node.
    tag: Option<u64>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// Property reading: a map's scalar entries (a list for an
    /// array of scalars), in document order.
    fields: Vec<(String, Value)>,
    /// Property reading: an array node's scalar elements, with
    /// their positions.
    items: Vec<(usize, Value)>,
}

/// How a document is read onto the arbor: the JSON adapter's two
/// readings, the same way here.
pub use quarb_json::Reading;

/// A Quarb adapter over a parsed CBOR document.
pub struct CborAdapter {
    nodes: Vec<Node>,
    root: NodeId,
    reading: Reading,
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
                calendar: Default::default(),
            })
        }
        (1, Cbor::Integer(i)) => Some(Value::Instant {
            secs: i128::from(*i) as i64,
            nanos: 0,
            offset_min: None,
            calendar: Default::default(),
        }),
        (1, Cbor::Float(f)) => Some(Value::Instant {
            secs: f.floor() as i64,
            nanos: ((f - f.floor()) * 1e9) as u32,
            offset_min: None,
            calendar: Default::default(),
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
    // remembers the number for `:::tag`.
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
                fields: Vec::new(),
                items: Vec::new(),
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
        fields: Vec::new(),
        items: Vec::new(),
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

/// The content a tag wraps, tags peeled (a time tag's content is a
/// scalar instant either way).
fn untagged(value: &Cbor) -> &Cbor {
    match value {
        Cbor::Tag(_, inner) => untagged(inner),
        v => v,
    }
}

fn is_container(value: &Cbor) -> bool {
    matches!(untagged(value), Cbor::Map(_) | Cbor::Array(_))
}

/// Whether `value` is a scalar, or an array of scalars — what the
/// property reading answers as a property.
fn is_property_shaped(value: &Cbor) -> bool {
    match untagged(value) {
        Cbor::Map(_) => false,
        Cbor::Array(items) => items.iter().all(|v| !is_container(v)),
        _ => true,
    }
}

/// A scalar's value under its tag (a time tag gives an instant).
fn scalar_value(value: &Cbor) -> Value {
    let mut nodes = Vec::new();
    build(value, None, None, None, &mut nodes);
    nodes[0].scalar.clone()
}

/// A scalar, or an array of scalars as a list.
fn property_value(value: &Cbor) -> Value {
    match untagged(value) {
        Cbor::Array(items) => Value::list(items.iter().map(scalar_value).collect()),
        _ => scalar_value(value),
    }
}

/// The property reading: containers are nodes, scalars are values
/// on them — the JSON adapter's `json2`, over CBOR.
fn build_property(
    value: &Cbor,
    name: Option<String>,
    parent: Option<NodeId>,
    nodes: &mut Vec<Node>,
) -> NodeId {
    let (tag, inner) = match value {
        Cbor::Tag(t, inner) => (Some(*t), untagged(inner)),
        v => (None, v),
    };
    let kind = match inner {
        Cbor::Map(_) => Kind::Map,
        _ => Kind::Array,
    };
    let root_list = parent.is_none() && is_property_shaped(value);
    let id = NodeId(nodes.len() as u64);
    nodes.push(Node {
        name,
        kind: if root_list && !matches!(inner, Cbor::Array(_)) {
            // A scalar document: the root wears its type.
            match inner {
                Cbor::Text(_) => Kind::String,
                Cbor::Integer(_) | Cbor::Float(_) => Kind::Number,
                Cbor::Bool(_) => Kind::Boolean,
                Cbor::Bytes(_) => Kind::Bytes,
                _ => Kind::Null,
            }
        } else {
            kind
        },
        scalar: if root_list {
            property_value(value)
        } else {
            Value::Null
        },
        bytes_len: None,
        tag,
        parent,
        children: Vec::new(),
        fields: Vec::new(),
        items: Vec::new(),
    });
    let mut children = Vec::new();
    let mut fields = Vec::new();
    let mut items = Vec::new();
    match inner {
        Cbor::Map(entries) => {
            for (k, v) in entries {
                if is_property_shaped(v) {
                    fields.push((key_name(k), property_value(v)));
                } else {
                    children.push(build_property(v, Some(key_name(k)), Some(id), nodes));
                }
            }
        }
        Cbor::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                if is_container(v) {
                    children.push(build_property(v, Some(i.to_string()), Some(id), nodes));
                } else if !root_list {
                    items.push((i, scalar_value(v)));
                }
            }
        }
        _ => {}
    }
    let n = &mut nodes[id.0 as usize];
    n.children = children;
    n.fields = fields;
    n.items = items;
    id
}

impl CborAdapter {
    /// Decode `bytes` as a single CBOR item and build the
    /// adapter.
    pub fn parse(bytes: &[u8]) -> Result<Self, CborError> {
        Self::parse_as(bytes, Reading::Node)
    }

    /// Decode under the given reading (`json2` is the property
    /// reading).
    pub fn parse_as(bytes: &[u8], reading: Reading) -> Result<Self, CborError> {
        let value: Cbor =
            ciborium::from_reader(bytes).map_err(|e| CborError::Decode(e.to_string()))?;
        let mut nodes = Vec::new();
        match reading {
            Reading::Node => build(&value, None, None, None, &mut nodes),
            Reading::Property => build_property(&value, None, None, &mut nodes),
        };
        Ok(CborAdapter {
            nodes,
            root: NodeId(0),
            reading,
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
    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        self.root
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
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

    /// The value type, and `scalar` over the primitive types
    /// (string, number, boolean, bytes, null, instant), as the
    /// JSON adapter has it.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let kind = self.nodes[node.0 as usize].kind;
        let mut out = vec![kind.name().to_string()];
        if !matches!(kind, Kind::Map | Kind::Array) {
            out.push("scalar".to_string());
        }
        out
    }

    /// A map's scalar entries answer as properties, like the
    /// JSON adapter's objects.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if self.reading == Reading::Property {
            return n
                .fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone());
        }
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

    /// Property reading: a map's scalar entries, an array's scalar
    /// elements under their index.
    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        if self.reading != Reading::Property {
            return Vec::new();
        }
        let n = &self.nodes[node.0 as usize];
        match n.kind {
            Kind::Map => n.fields.clone(),
            Kind::Array => n
                .items
                .iter()
                .map(|(i, v)| (i.to_string(), v.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        // Property reading: a mixed array projects its scalar
        // elements as a list.
        if self.reading == Reading::Property && !n.items.is_empty() {
            return Some(Value::list(
                n.items.iter().map(|(_, v)| v.clone()).collect(),
            ));
        }
        match &n.scalar {
            Value::Null => None,
            v => Some(v.clone()),
        }
    }

    /// `:::type`; `:::length`, a count (entries for containers,
    /// characters for text); `:::size`, a byte string's raw byte
    /// count as the size quantity (spec: adapter metadata, draft
    /// 157: a size is a quantity in bytes, a length a count); and
    /// `:::tag` (the enclosing CBOR tag number, when any).
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["type", "size", "length", "tag"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "type" => Some(Value::Str(n.kind.name().to_string())),
            "size" => match n.kind {
                Kind::Bytes => Some(Value::bytes(n.bytes_len? as i64)),
                _ => None,
            },
            "length" => match n.kind {
                Kind::Map | Kind::Array => Some(Value::Int(
                    (n.children.len() + n.fields.len() + n.items.len()) as i64,
                )),
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

    /// The property reading: the map is a node, its scalar entries
    /// (the tagged instant and the bytes included) are its
    /// properties, and no scalar is a node.
    #[test]
    fn property_reading() {
        let a = CborAdapter::parse_as(&music(), Reading::Property).unwrap();
        assert_eq!(v(&a, "*//*"), ["/", "/tracks", "/tracks/0"]);
        assert_eq!(v(&a, "//title @| count"), ["0"]);
        assert_eq!(v(&a, "/tracks/*::title"), ["Mars"]);
        assert_eq!(v(&a, "/tracks/*::price"), ["1.29"]);
        assert_eq!(v(&a, "/tracks/*::art"), ["c0ffee"]);
        assert_eq!(v(&a, "/tracks/*::42"), ["int-keyed"]);
        assert_eq!(v(&a, "/tracks/*::added | year"), ["2023"]);
        assert_eq!(v(&a, "/tracks/0:::length"), ["5"]);
    }

    #[test]
    fn navigates_like_json() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/title::"), ["Mars"]);
        assert_eq!(v(&a, "/tracks/0::price"), ["1.29"]);
        assert_eq!(v(&a, "/tracks:::length"), ["1"]);
    }

    #[test]
    fn time_tags_become_instants() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/added:::type"), ["instant"]);
        assert_eq!(v(&a, "/tracks/*[::added > 2023-01-01] @| count"), ["1"]);
        assert_eq!(v(&a, "/tracks/0/added:::tag"), ["1"]);
    }

    #[test]
    fn bytes_render_hex_with_raw_length() {
        let a = CborAdapter::parse(&music()).unwrap();
        assert_eq!(v(&a, "/tracks/0/art::"), ["c0ffee"]);
        // a byte string's raw byte count is its size, a quantity;
        // length is the count of entries or characters elsewhere
        assert_eq!(v(&a, "/tracks/0/art:::size"), ["3_B"]);
        assert_eq!(v(&a, "/tracks/0/art:::length"), [""]);
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
        assert_eq!(v(&a, "/when:::type"), ["instant"]);
    }
}
